#![cfg(unix)]

#[path = "cli_flow/acp.rs"]
mod acp;

#[path = "cli_flow/permission.rs"]
mod permission;
#[path = "cli_flow/question.rs"]
mod question;
#[path = "cli_flow/tasks.rs"]
mod tasks;

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("dim-cli-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("rust")).unwrap();
        fs::set_permissions(path.join("rust"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(path.join("config.json"), r#"{"locale":"en"}"#).unwrap();
        Self(path)
    }
    fn daemon(&self, flow: impl FnOnce(&mut Wire) + Send + 'static) -> thread::JoinHandle<()> {
        let path = self.0.join("rust/daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut wire = Wire(BufReader::new(stream));
            flow(&mut wire);
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dimcode"));
        command.args(["--home", self.0.to_str().unwrap()]);
        command.env("DIMCODE_UPDATE_CHECK", "0");
        command
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// How long a step that should succeed may take before the test fails. CI
/// shares one machine between jobs, and a debug CLI starting under that load
/// can take several seconds; a passing step never waits this long.
const PATIENCE: Duration = Duration::from_secs(30);

struct Wire(BufReader<UnixStream>);
impl Wire {
    fn read(&mut self, method: &str) -> Value {
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["method"], method);
        value
    }
    /// The next request, whichever it is: for requests whose order the
    /// flow does not fix.
    fn read_any(&mut self) -> Value {
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    fn send(&mut self, value: Value) {
        writeln!(self.0.get_mut(), "{value}").unwrap();
        self.0.get_mut().flush().unwrap();
    }
    fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
    }
    fn notice(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }
    fn quiet(&mut self, duration: Duration) {
        let previous = self.0.get_ref().read_timeout().unwrap();
        self.0.get_ref().set_read_timeout(Some(duration)).unwrap();
        let mut line = String::new();
        let result = self.0.read_line(&mut line);
        self.0.get_ref().set_read_timeout(previous).unwrap();
        assert!(
            matches!(result, Err(ref error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "Unexpected request or closure: {line:?}, {result:?}"
        );
    }
    fn start(&mut self, resume: bool) {
        self.hello(&[]);
        if !resume {
            self.created();
        }
        self.attached();
    }
    /// Answers `hello`, advertising `extra` methods besides the Session basics.
    fn hello(&mut self, extra: &[&str]) {
        let hello = self.read("hello");
        assert_eq!(hello["params"]["protocolVersion"], 3);
        let mut features = vec!["session.create", "session.history", "provider.list"];
        features.extend(extra);
        self.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}));
    }
    fn created(&mut self) -> Value {
        let create = self.read("session.create");
        self.reply(&create, json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        create
    }
    fn attached(&mut self) {
        let attach = self.read("session.attach");
        self.reply(&attach, json!({}));
        let history = self.read("session.history");
        self.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let idle = self.read("session.idle");
        self.reply(&idle, json!({"sequence":8}));
    }
    fn event(&mut self, sequence: u64, name: &str, data: Value) {
        self.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":format!("event-{sequence}"),"sequence":sequence,"create_at":1,"type":"step","name":name,"data":data}}));
    }
    fn end(&mut self, outcome: Value) {
        self.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"end","sequence":25,"create_at":1,"type":"turn","name":"end","data":outcome}}));
    }
    fn detach(&mut self) {
        let detach = self.read("session.detach");
        self.reply(&detach, json!({}));
    }
}

fn wait(mut child: Child) -> Output {
    let deadline = Instant::now() + PATIENCE;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("CLI did not settle");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn help_and_version_do_not_create_product_state() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for flag in ["--help", "--version"] {
        let output = home.command().arg(flag).output().unwrap();
        assert!(output.status.success());
    }
    assert!(!home.0.join("rust").exists());
}

#[test]
fn model_catalog_lists_safe_ids_and_availability_and_refreshes_explicitly() {
    for refresh in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
            let request = wire.read(if refresh { "provider.refresh" } else { "provider.catalog" });
            assert_eq!(request["params"], json!({"providerId":"fixture"}));
            let catalog = json!({"account":{"accessToken":"must-not-print"},"version":1,"disabledModelIds":["disabled"],"models":[
                {"id":"model\u{1b}[2J-id","name":"Model\nName\tvalue","unavailableReason":null},
                {"id":"disabled","name":"Disabled","unavailableReason":null},
                {"id":"missing","name":"Missing","unavailableReason":"private diagnostic must-not-print"}
            ]});
            wire.reply(&request, if refresh { catalog } else { json!({"catalog":catalog}) });
        });
        let mut command = home.command();
        command.args(["models", "fixture"]);
        if refresh {
            command.arg("--refresh");
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "model-id\tModel Name value\tavailable\ndisabled\tDisabled\tdisabled\nmissing\tMissing\tunavailable\n"
        );
        assert!(output.stderr.is_empty());
    }
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let catalog = wire.read("provider.catalog");
        wire.reply(&catalog, json!({"catalog":null}));
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--refresh"));
}

#[test]
fn daemon_stop_without_an_endpoint_never_creates_state_or_spawns_a_server() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let output = home
        .command()
        .args([
            "--server",
            "/missing-app-server",
            "--model-config",
            "/missing-model-config",
            "daemon",
            "stop",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.0.join("rust").exists());
    fs::create_dir(home.0.join("rust")).unwrap();
    fs::set_permissions(home.0.join("rust"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        home.command()
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!home.0.join("rust/daemon.log").exists());
}

#[test]
fn daemon_stop_waits_for_completion_and_closure_without_touching_a_successor() {
    let home = Home::new();
    fs::write(home.0.join("account-marker"), "retained").unwrap();
    let socket = home.0.join("rust/daemon.sock");
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"configured","modelConfigSha256":"0".repeat(64)},"features":["session.create","session.history","daemon.shutdown.completed"]}));
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(shutdown["params"], json!({}));
        thread::sleep(Duration::from_millis(100));
        fs::remove_file(&socket).unwrap();
        let _successor = UnixListener::bind(&socket).unwrap();
        wire.reply(&shutdown, json!({}));
        thread::sleep(Duration::from_millis(100));
        wire.0.get_mut().shutdown(std::net::Shutdown::Write).unwrap();
    });
    let started = Instant::now();
    let output = home.command().args(["daemon", "stop"]).output().unwrap();
    let elapsed = started.elapsed();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elapsed >= Duration::from_millis(200));
    assert!(home.0.join("rust/daemon.sock").exists());
    assert_eq!(
        fs::read_to_string(home.0.join("account-marker")).unwrap(),
        "retained"
    );
}

#[test]
fn a_daemon_of_another_version_is_stopped_and_replaced() {
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"old","serverVersion":"0.0.1-old","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","daemon.shutdown.completed"]}));
        let shutdown = wire.read("daemon.shutdown");
        fs::remove_file(&socket).unwrap();
        let successor = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        successor.set_nonblocking(true).unwrap();
        wire.reply(&shutdown, json!({}));
        wire.0.get_mut().shutdown(std::net::Shutdown::Write).unwrap();
        let deadline = Instant::now() + PATIENCE;
        loop {
            let stream = match successor.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "the CLI never reconnected");
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            let mut wire = Wire(BufReader::new(stream));
            let mut line = String::new();
            if wire.0.read_line(&mut line).unwrap() == 0 {
                continue;
            }
            let hello: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(hello["method"], "hello");
            wire.reply(&hello,json!({"instanceId":"new","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
            let catalog = wire.read("provider.catalog");
            wire.reply(&catalog, json!({"catalog":null}));
            break;
        }
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn daemon_stop_never_treats_old_protocol_eof_or_cleanup_failure_as_completion() {
    for case in ["old", "eof", "failed"] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            let mut features = vec!["session.create", "session.history"];
            if case != "old" { features.push("daemon.shutdown.completed"); }
            wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}));
            if case == "old" {
                let mut line = String::new();
                assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "must not stop an incompatible daemon");
            } else {
                let request = wire.read("daemon.shutdown");
                if case == "failed" { wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"Daemon cleanup failed"}})); }
            }
        });
        let output = home.command().args(["daemon", "stop"]).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{case}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Daemon stopped."));
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn headless_streams_unicode_once_and_waits_for_the_delivery_barrier() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "/fast on");
        assert!(!submit["params"]["submissionId"].as_str().unwrap().is_empty());
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":0,"text":"你\u{1b}[2J好","thinking":"","thinkingOffset":0}));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"你\u{1b}[2J好"}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":6,"text":"世界","thinking":"","thinkingOffset":0}));
        wire.event(20,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"世界"}));
        wire.event(22,"model-output-end",json!({"turn_id":"turn","request_id":"model-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["--prompt", "/fast on"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "你好世界\n");
}

fn image_fixture(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn exec_images_preserve_format_bytes_order_and_invoking_directory() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    for source in [
        "images",
        "prompt",
        "empty",
        "stdin",
        "whitespace",
        "whitespace_prompt",
        "symlink",
        "twenty",
        "terminal",
    ] {
        let home = Home::new();
        let workspace = home.0.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut images = Vec::new();
        let mut paths = Vec::new();
        for (index, (format, media_type)) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::WebP, "image/webp"),
            (image::ImageFormat::Gif, "image/gif"),
        ]
        .into_iter()
        .enumerate()
        {
            let bytes = image_fixture(format, 1, if index == 0 { 8000 } else { 1 });
            let path = format!("image {index}.txt");
            fs::write(home.0.join(&path), &bytes).unwrap();
            images.push(json!({"type":"image","source":{"type":"base64","mediaType":media_type,"data":STANDARD.encode(bytes)}}));
            paths.push(path);
        }
        if source == "symlink" {
            let link = home.0.join("linked.png");
            std::os::unix::fs::symlink(home.0.join(&paths[0]), &link).unwrap();
            paths[0] = link.to_str().unwrap().into();
        }
        if source == "twenty" {
            images = images.iter().cycle().take(20).cloned().collect();
            paths = paths.iter().cycle().take(20).cloned().collect();
        }
        let text = match source {
            "prompt" => Some("compare /exit literally"),
            "empty" => Some(""),
            "stdin" => Some("stdin\r\nwith trailing newline\n"),
            "whitespace" | "whitespace_prompt" => Some(" \t\n"),
            _ => None,
        };
        let mut expected = Vec::new();
        if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
            expected.push(json!({"type":"text","text":text}));
        }
        expected.extend(images);
        let daemon = home.daemon(move |wire| {
            wire.start(source == "prompt");
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["user"]["content"], json!(expected));
            assert!(submit["params"].get("text").is_none());
            assert_eq!(submit["params"]["mode"], "queue");
            uuid::Uuid::parse_str(submit["params"]["submissionId"].as_str().unwrap()).unwrap();
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":30}));
            wire.detach();
        });
        let mut command = home.command();
        command
            .current_dir(&home.0)
            .args(["--cwd", workspace.to_str().unwrap(), "exec"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if source == "prompt" {
            command.args(["resume", "session", "--json"]);
        }
        if matches!(source, "prompt" | "empty" | "whitespace_prompt") {
            command.args(["-p", text.unwrap()]);
        }
        if matches!(source, "stdin" | "whitespace") {
            command.stdin(Stdio::piped());
        }
        for path in paths {
            command.arg("--image").arg(path);
        }
        let output = if source == "terminal" {
            Output {
                status: Terminal::launch(command).wait(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        } else {
            let mut child = command.spawn().unwrap();
            if matches!(source, "stdin" | "whitespace") {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(text.unwrap().as_bytes())
                    .unwrap();
            }
            wait(child)
        };
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn invalid_exec_images_fail_before_connecting_or_creating_home() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let png = image_fixture(image::ImageFormat::Png, 1, 1);
    let mut bad_crc = png.clone();
    bad_crc[29] ^= 1;
    let mut oversized = png.clone();
    oversized.resize(5 * 1024 * 1024 + 1, 0);
    let mut zero = image_fixture(image::ImageFormat::Gif, 1, 1);
    zero[6..8].fill(0);
    for (name, bytes, reason) in [
        ("text", b"not an image".to_vec(), "PNG, JPEG, WebP or GIF"),
        ("truncated", png[..8].to_vec(), "Invalid or truncated"),
        (
            "png_header_only",
            png[..33].to_vec(),
            "Invalid or truncated",
        ),
        ("jpeg", b"\xff\xd8\xff".to_vec(), "Invalid or truncated"),
        ("gif", b"GIF89a".to_vec(), "Invalid or truncated"),
        ("webp", b"RIFF\0\0\0\0WEBP".to_vec(), "Invalid or truncated"),
        ("zero", zero, "dimensions"),
        ("crc", bad_crc, "Invalid or truncated"),
        (
            "wide",
            image_fixture(image::ImageFormat::Png, 8001, 1),
            "8000",
        ),
        (
            "tall",
            image_fixture(image::ImageFormat::Gif, 1, 8001),
            "8000",
        ),
        (
            "jpeg_wide",
            image_fixture(image::ImageFormat::Jpeg, 8001, 1),
            "8000",
        ),
        (
            "webp_tall",
            image_fixture(image::ImageFormat::WebP, 1, 8001),
            "8000",
        ),
        ("large", oversized, "5 MiB"),
    ] {
        let file = home.0.join(name);
        fs::write(&file, bytes).unwrap();
        let output = home
            .command()
            .arg("exec")
            .arg("--image")
            .arg(file)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!home.0.join("rust").exists());
    }
    let fifo = home.0.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for args in [
        vec!["--image", "missing.png"],
        vec!["tui", "--image", "missing.png"],
        vec!["exec", "--image"],
        vec!["exec", "--image", "missing.png"],
        vec!["exec", "--image", "http://127.0.0.1:9/missing.png"],
        vec!["exec", "--image", home.0.to_str().unwrap()],
        vec!["exec", "--image", fifo.to_str().unwrap()],
        std::iter::once("exec")
            .chain(["--image", "missing.png"].repeat(21))
            .collect(),
    ] {
        let output = home
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!home.0.join("rust").exists());
    }
}

#[test]
fn exec_images_check_the_combined_encoded_frame_before_sending() {
    for size in [5 * 1024 * 1024, 4 * 1024 * 1024 - 3] {
        let home = Home::new();
        let early = size > 4 * 1024 * 1024;
        let mut png = image_fixture(image::ImageFormat::Png, 1, 1);
        png.resize(size, 0);
        let image = home.0.join("large.png");
        fs::write(&image, png).unwrap();
        let daemon = if early {
            fs::remove_dir_all(home.0.join("rust")).unwrap();
            None
        } else {
            Some(home.daemon(|wire| {
                wire.start(false);
                wire.detach();
            }))
        };
        let mut command = home.command();
        command.arg("exec").stdin(Stdio::null());
        for _ in 0..3 {
            command.arg("--image").arg(&image);
        }
        let output = command.output().unwrap();
        if let Some(daemon) = daemon {
            daemon.join().unwrap();
        } else {
            assert!(!home.0.join("rust").exists());
        }
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("16 MiB"));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn submit_errors_on_a_live_connection_exit_without_resending() {
    for (app_error, uncertain) in [
        (Some("submission_unknown"), true),
        (Some("publication_failed"), true),
        (None, true),
        (Some("session_invalid_input"), false),
        (Some("invalid_params"), false),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            if uncertain {
                wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"committed","sequence":9,"create_at":1,"type":"input","name":"message","data":{"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"once"}]},"mode":"queue"}}}));
            }
            wire.send(json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":-32000,"message":"Submission failed","data":app_error.map(|code| json!({"appError":code}))}}));
            wire.detach();
        });
        let output = home
            .command()
            .args(["exec", "-p", "once"])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("Submission failed"));
        if uncertain {
            assert!(diagnostic.contains("session: session"));
            assert!(diagnostic.contains("input may already be queued"));
            assert!(diagnostic.contains("instead of sending it again"));
        } else {
            assert!(!diagnostic.contains("input may already be queued"));
            assert!(!diagnostic.contains("instead of sending it again"));
        }
    }
}

#[test]
fn noninteractive_approval_denies_and_drains_outstanding_responses_before_detach() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"do something","token":"never print me"}},"reason":"shell command"}));
        let approval = wire.read("approval.respond"); assert_eq!(approval["params"]["decision"],"deny");
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"denied"}));
        wire.reply(&stop,json!({"sequence":30}));
        wire.reply(&idle,json!({"sequence":30}));
        wire.reply(&approval,json!({}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--json", "--prompt", "exec"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("never print me"));
    let notices: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0]["method"], "approval.request");
    assert_eq!(notices[1]["method"], "session.event");
}

#[test]
fn resume_advances_filtered_history_positions_and_does_not_reprint_old_output() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let first = wire.read("session.history"); assert_eq!(first["params"]["after"],0);
        wire.reply(&first,json!({"events":[{"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old-request","delta":"old output"}}],"through":10,"nextAfter":7,"done":false}));
        let next = wire.read("session.history");
        assert_eq!(next["params"]["after"],7); assert_eq!(next["params"]["through"],10);
        wire.reply(&next,json!({"events":[{"sequence":8,"create_at":2,"type":"step","name":"model-output-end","data":{"turn_id":"old-turn","request_id":"old-request","status":"completed","finish_reason":"end_turn"}}],"through":10,"nextAfter":10,"done":true}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"old-request","textOffset":0,"text":"old output","thinking":"","thinkingOffset":0}));
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":10}));
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"new-turn","request_id":"new-request","delta":"new output"}));
        wire.end(json!({"turn_id":"new-turn","status":"completed","reason":"done"}));
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "resume", "session", "--prompt", "continue"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "new output\n");
}

#[test]
fn exec_json_preserves_public_notifications_and_only_observes_the_selected_session() {
    let home = Home::new();
    let notices = vec![
        json!({"jsonrpc":"2.0","method":"session.mcp.state","params":{"sessionId":"session","servers":[{"id":"docs","state":"auth_required"}]}}),
        json!({"jsonrpc":"2.0","method":"session.plugin.notice","params":{"sessionId":"session","branchId":"child","source":"fixture","future":{"control":"\u{1b}[2J\n"}}}),
        json!({"jsonrpc":"2.0","method":"session.model.progress","params":{"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"Hello","newField":true}}),
        json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":12,"create_at":1,"type":"step","name":"text","data":{"turn_id":"turn","request_id":"request","delta":"Hello","newField":"preserved"}}}}),
        json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":25,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"turn","status":"failed","error":"fixture"}}}}),
    ];
    let emitted = notices.clone();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[{"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old","delta":"history-only","newField":"preserved"}}],"through":8,"nextAfter":8,"done":true}));
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "continue"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.closed",json!({"sessionId":"other"}));
        wire.notice("session.model.progress",json!({"sessionId":"other","text":"other-session"}));
        for notice in emitted { wire.send(notice); }
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "resume", "session", "--json", "-p", "continue"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let received: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(received, notices);
    assert!(!stdout.contains("history-only"));
    assert!(!stdout.contains("workspace:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("session: session"));
}

#[test]
fn f6_argument_errors_do_not_connect_or_create_product_state() {
    let home = Home::new();
    for args in [
        vec!["exec", "--prompt", "one", "-p", "two"],
        vec!["exec", "resume", "one", "--resume", "two", "-p", "text"],
        vec!["--resume", "one", "exec", "resume", "two", "-p", "text"],
        vec!["--json", "-p", "text"],
        vec!["session", "export", "session"],
        vec!["session", "export", "session", "--format", "xml"],
        vec!["tui"],
        vec!["exec"],
        vec!["exec", "--permission", "admin", "-p", "text"],
        vec!["exec", "--permission", "-p", "text"],
        vec![
            "--permission",
            "read-only",
            "--permission",
            "full-access",
            "-p",
            "text",
        ],
        vec!["mcp", "list", "--permission", "read-only"],
        vec!["export"],
        vec!["export", "session", "--format", "json"],
        vec!["--database", "store.sqlite", "export", "session"],
        vec!["export", "session", "--prompt", "text"],
        vec![
            "session",
            "export",
            "session",
            "--format",
            "json",
            "--permission",
            "full-access",
        ],
    ] {
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(!home.0.join("rust/daemon.sock").exists());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn exec_json_interrupt_drains_the_stop_barrier_without_human_stdout() {
    let home = Home::new();
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        entered.send(()).unwrap();
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.detach();
    });
    let child = home
        .command()
        .args(["exec", "-p", "wait", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_entered.recv_timeout(PATIENCE).unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let events: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["method"], "session.event");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Waiting for resource settlement"));
}

#[test]
fn session_export_preserves_the_fixed_main_prefix_and_does_not_replace_files() {
    for mode in ["stdout-json", "markdown", "existing", "history-error"] {
        let home = Home::new();
        let cli_dir = home.0.join("client");
        let workspace = home.0.join("workspace");
        fs::create_dir(&cli_dir).unwrap();
        fs::create_dir(&workspace).unwrap();
        let path = cli_dir.join("Session notes.md");
        if mode == "existing" {
            fs::write(&path, b"keep original bytes").unwrap();
        }
        let events = vec![
            json!({"sequence":1,"create_at":1,"type":"extension","name":"future","data":{"unknown":[1,2]},"extra":"preserved"}),
            json!({"sequence":2,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"Keep ```code``` intact"},{"type":"image","source":{"type":"url","mediaType":"image/png","url":"https://invalid.example/not-fetched"}}]}}}),
            json!({"sequence":3,"create_at":1,"type":"step","name":"thinking","data":{"turn_id":"t","request_id":"r","delta":"A reason"}}),
            json!({"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"Hello "}}),
            json!({"sequence":5,"create_at":1,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"world"}}),
            json!({"sequence":6,"create_at":1,"type":"step","name":"tool-use","data":{"turn_id":"t","request_id":"r","call":{"id":"call","name":"read","input":{"path":"file"}}}}),
            json!({"sequence":7,"create_at":1,"type":"step","name":"tool-completed","data":{"turn_id":"t","request_id":"r","call_event_id":"dispatch","call_id":"call","result":{"content":"```\n# must remain inside fence\n``````"}}}),
            json!({"sequence":8,"create_at":1,"type":"step","name":"tool-result","data":{"turn_id":"t","request_id":"r","results":[{"call_id":"call","result":{"content":"```\n# must remain inside fence\n``````"}}]}}),
            json!({"sequence":9,"create_at":1,"type":"branch","name":"create","data":{"branch":"child","at":{"kind":"create"},"source":{"kind":"host","operation_id":"export-fixture"},"prompt":"child contents are elsewhere"}}),
            json!({"sequence":10,"create_at":1,"type":"branch","name":"create-failed","data":{"status":"failed","branch":"child","createEvent":9,"error":"fixture"}}),
            json!({"sequence":15,"create_at":1,"type":"branch","name":"create-failed","data":{"status":"cancelled","branch":"cancelled-child","createEvent":14,"reason":"fixture"}}),
        ];
        let returned = events.clone();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
            let first = wire.read("session.history");
            assert_eq!(first["params"],json!({"sessionId":"session","branchId":"main","after":0,"limit":256}));
            wire.reply(&first,json!({"events":&returned[..10],"through":20,"nextAfter":12,"done":false}));
            let second = wire.read("session.history");
            assert_eq!(second["params"]["after"],12); assert_eq!(second["params"]["through"],20);
            if mode == "history-error" {
                wire.send(json!({"jsonrpc":"2.0","id":second["id"],"error":{"code":-32000,"message":"fixture history failure"}}));
            } else {
                wire.reply(&second,json!({"events":&returned[10..],"through":20,"nextAfter":20,"done":true}));
            }
        });
        let mut command = home.command();
        command.current_dir(&cli_dir).args([
            "session",
            "export",
            "session",
            "--format",
            if mode == "stdout-json" {
                "json"
            } else {
                "markdown"
            },
            "--cwd",
            workspace.to_str().unwrap(),
        ]);
        if mode != "stdout-json" {
            command.args(["--output", "Session notes.md"]);
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        match mode {
            "stdout-json" => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let exported: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(
                    exported,
                    json!({"sessionId":"session","branchId":"main","through":20,"events":events})
                );
            }
            "markdown" => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.stdout.is_empty());
                let markdown = fs::read_to_string(&path).unwrap();
                for text in [
                    "Keep ```code``` intact",
                    "[Image attachment: image/png]",
                    "### Reasoning",
                    "Hello world",
                    "### Tool call: read (call)",
                    "### Tool result: call",
                    "```````json",
                    "Child branch child (contents not exported).",
                    "Child branch child was not created.",
                    "Child branch cancelled-child was not created.",
                ] {
                    assert!(markdown.contains(text), "{text}: {markdown}");
                }
                assert_eq!(markdown.matches("# must remain inside fence").count(), 1);
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            "existing" => {
                assert!(!output.status.success());
                assert_eq!(fs::read(&path).unwrap(), b"keep original bytes");
                assert!(!String::from_utf8_lossy(&output.stderr).contains("Exported"));
            }
            _ => {
                assert!(!output.status.success());
                assert!(!path.exists());
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("fixture history failure")
                );
            }
        }
        assert!(!workspace.join("Session notes.md").exists());
    }
}

#[test]
fn interactive_exports_keep_literal_paths_and_errors_out_of_model_input() {
    let home = Home::new();
    let cli_dir = home.0.join("client");
    let workspace = home.0.join("workspace");
    fs::create_dir_all(cli_dir.join("~")).unwrap();
    fs::create_dir(&workspace).unwrap();
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        for index in 0..2 {
            let history = wire.read("session.history");
            if index == 0 {
                entered.send(()).unwrap();
                wait_release.recv_timeout(PATIENCE).unwrap();
            }
            wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        }
        let history = wire.read("session.history");
        wire.send(json!({"jsonrpc":"2.0","id":history["id"],"error":{"code":-32000,"message":"fixture history failure"}}));
        wire.detach();
    });
    let mut command = home.command();
    command
        .current_dir(&cli_dir)
        .args(["tui", "--cwd", workspace.to_str().unwrap()]);
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"/export markdown\n");
    terminal.until("~ is literal.");
    terminal.until("\n> ");
    terminal.write(b"/export yaml bad.md\n");
    terminal.until("Export format must be markdown or json");
    terminal.until("\n> ");
    terminal.write(b"/export json ~/Session notes.json\n");
    wait_entered.recv_timeout(PATIENCE).unwrap();
    for _ in 0..2 {
        terminal.signal(libc::SIGINT);
        terminal.until("Waiting for export result");
    }
    release.send(()).unwrap();
    terminal.until("Exported JSON: ~/Session notes.json");
    terminal.until("\n> ");
    terminal.write(b"/export json ~/Session notes.json\n");
    terminal.until("Error:");
    terminal.until("\n> ");
    terminal.write(b"/export markdown failure.md\n");
    terminal.until("fixture history failure");
    terminal.until("\n> ");
    terminal.write(b"/exit extra\n");
    terminal.until("Usage: /exit");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let path = cli_dir.join("~/Session notes.json");
    let data: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(data["events"], json!([]));
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!cli_dir.join("failure.md").exists());
    assert!(!workspace.join("~").exists());
}

#[test]
fn incompatible_daemon_is_rejected_without_removing_its_endpoint() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            json!({"instanceId":"other","serverVersion":"1","protocolVersion":2,"features":[]}),
        );
    });
    let output = home.command().args(["--prompt", "hello"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(home.0.join("rust/daemon.sock").exists());
    assert!(!home.0.join("rust/daemon.log").exists());
}

#[test]
fn explicit_model_config_rejects_mock_and_mismatched_configured_profiles() {
    for profile in [
        json!({"kind":"mock"}),
        json!({"kind":"configured","modelConfigSha256":"0".repeat(64)}),
    ] {
        let home = Home::new();
        let config = home.0.join("model.json");
        fs::write(&config, "{}").unwrap();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":profile,"features":["session.create","session.history"]}));
        });
        let output = home
            .command()
            .args([
                "--model-config",
                config.to_str().unwrap(),
                "--prompt",
                "hello",
            ])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("different model profile"));
        assert!(output.stdout.is_empty());
        assert!(home.0.join("rust/daemon.sock").exists());
        assert!(!home.0.join("rust/daemon.log").exists());
    }
}

#[test]
fn piped_prompt_runs_once_and_model_failure_has_a_distinct_exit_code() {
    for (explicit_exec, input) in [
        (false, "from stdin\n"),
        (true, "from stdin\n"),
        (false, "/mcp login docs\n"),
        (true, "/mcp login docs\n"),
        (false, "!!literal prompt\n"),
        (true, "!!literal prompt\n"),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], input);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.end(
                json!({"turn_id":"turn","status":"failed","error":"credential should not print"}),
            );
            wire.reply(&idle, json!({"sequence":30}));
            wire.detach();
        });
        let mut command = home.command();
        if explicit_exec {
            command.arg("exec");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = wait(child);
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("credential should not print"));
    }
    for explicit_exec in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.start(false);
            wire.detach();
        });
        let mut command = home.command();
        if explicit_exec {
            command.arg("exec");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&vec![b'"'; 8 * 1024 * 1024])
            .unwrap();
        let output = wait(child);
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("App Server request exceeds 16 MiB after JSON encoding")
        );
    }
}

/// A CLI running on its own pseudo-terminal. Dropping it before the CLI has
/// exited, as a failed flow does, ends the CLI's whole process group.
struct Terminal {
    master: Option<fs::File>,
    child: Child,
    transcript: String,
    consumed: usize,
}
impl Terminal {
    fn editor(&mut self) {
        self.until("\u{1b}[?2004h");
        self.until("Ctrl+J: newline");
    }
    /// Launches under a pty with the line client; a dimcode chat in a pty
    /// otherwise opens the terminal UI.
    fn launch(mut command: Command) -> Self {
        if command.get_program() == env!("CARGO_BIN_EXE_dimcode") {
            command.arg("--no-tui");
        }
        Self::launch_tui(command)
    }
    fn launch_tui(mut command: Command) -> Self {
        let master = unsafe {
            libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
        };
        assert!(master >= 0, "{}", std::io::Error::last_os_error());
        let master = unsafe { fs::File::from_raw_fd(master) };
        let fd = master.as_raw_fd();
        assert_eq!(unsafe { libc::grantpt(fd) }, 0);
        assert_eq!(unsafe { libc::unlockpt(fd) }, 0);
        let mut name = [0; 128];
        #[cfg(target_os = "macos")]
        let named = unsafe { libc::ioctl(fd, libc::TIOCPTYGNAME as _, name.as_mut_ptr()) };
        #[cfg(not(target_os = "macos"))]
        let named = unsafe { libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) };
        assert_eq!(named, 0);
        let slave = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        assert!(slave >= 0, "{}", std::io::Error::last_os_error());
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut command, || {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            master: Some(master),
            child: command.spawn().unwrap(),
            transcript: String::new(),
            consumed: 0,
        }
    }
    fn signal(&self, signal: libc::c_int) {
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
    }
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            assert!(Instant::now() < deadline, "CLI did not settle");
            let mut bytes = [0u8; 4096];
            match self.master.as_mut().unwrap().read(&mut bytes) {
                Ok(size) if size > 0 => {
                    self.transcript
                        .push_str(&String::from_utf8_lossy(&bytes[..size]));
                    continue;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error)
                    if cfg!(target_os = "linux") && error.raw_os_error() == Some(libc::EIO) => {}
                Err(error) => panic!("{error}"),
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn until(&mut self, expected: &str) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(index) = self.transcript[self.consumed..].find(expected) {
                self.consumed += index + expected.len();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Terminal did not show {expected:?}: {}",
                self.transcript
            );
            let mut poll = libc::pollfd {
                fd: self.master.as_ref().unwrap().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 100) } == 0 {
                continue;
            }
            let mut bytes = [0u8; 4096];
            let size = match self.master.as_mut().unwrap().read(&mut bytes) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                result => result.unwrap(),
            };
            assert!(
                size > 0,
                "Terminal closed before {expected}: {}",
                self.transcript
            );
            self.transcript
                .push_str(&String::from_utf8_lossy(&bytes[..size]));
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        self.raw(
            &bytes
                .iter()
                .map(|&byte| if byte == b'\n' { b'\r' } else { byte })
                .collect::<Vec<_>>(),
        );
    }
    fn raw(&mut self, mut bytes: &[u8]) {
        let deadline = Instant::now() + PATIENCE;
        while !bytes.is_empty() {
            assert!(Instant::now() < deadline, "Terminal did not accept input");
            match self.master.as_mut().unwrap().write(bytes) {
                Ok(0) => panic!("Terminal input closed"),
                Ok(size) => bytes = &bytes[size..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut output = [0u8; 4096];
                    match self.master.as_mut().unwrap().read(&mut output) {
                        Ok(0) => panic!("Terminal output closed while writing"),
                        Ok(size) => self
                            .transcript
                            .push_str(&String::from_utf8_lossy(&output[..size])),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => panic!("{error}"),
                    }
                    let mut poll = libc::pollfd {
                        fd: self.master.as_ref().unwrap().as_raw_fd(),
                        events: libc::POLLIN | libc::POLLOUT,
                        revents: 0,
                    };
                    assert!(unsafe { libc::poll(&mut poll, 1, 100) } >= 0);
                }
                Err(error) => panic!("{error}"),
            }
        }
    }
    fn paste(&mut self, text: &str) {
        self.raw(b"\x1b[200~");
        self.raw(text.as_bytes());
        self.raw(b"\x1b[201~");
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            drop(self.master.take());
            let _ = self.child.wait();
        }
    }
}

#[test]
fn a_failed_terminal_flow_leaves_no_cli_running() {
    for pending_output in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
        wire.start(false);
        if pending_output {
            let submit = wire.read("session.submit");
            wire.reply(&submit, json!({"event":{}}));
            let _idle = wire.read("session.idle");
            wire.event(12, "text", json!({"turn_id":"turn","request_id":"request","delta":format!("pending output {}", "x".repeat(512 * 1024))}));
        }
        let mut rest = Vec::new();
        wire.0.read_to_end(&mut rest).unwrap();
    });
        let flow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut terminal = Terminal::launch(home.command());
            terminal.editor();
            if pending_output {
                terminal.write(b"continue\n");
                terminal.until("pending output");
                thread::sleep(Duration::from_millis(150));
            }
            panic!("the flow fails while its CLI waits for input");
        }));
        assert_eq!(
            flow.unwrap_err().downcast_ref::<&str>(),
            Some(&"the flow fails while its CLI waits for input")
        );
        daemon.join().unwrap();
    }
}

#[test]
#[cfg(target_os = "linux")]
fn clipboard_cancellation_keeps_raw_terminal_input_responsive_and_reaps_the_reader() {
    let home = Home::new();
    let bin = home.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let helper = bin.join("wl-paste");
    fs::write(&helper, "#!/bin/sh\nif [ \"$1\" = \"--list-types\" ]; then\n  printf 'text/plain;charset=utf-8\\n'\n  exit 0\nfi\nprintf '%s\\n' \"$$\" > \"$DIM_CLIPBOARD_TEST_PID\"\nexec /bin/sleep 300\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let pid_file = home.0.join("reader.pid");
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for text in ["after cancel", "retained"] {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], text);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":25}));
        }
        wire.detach();
    });
    let mut command = home.command();
    command
        .env("PATH", &bin)
        .env("WAYLAND_DISPLAY", "fixture")
        .env("DIM_CLIPBOARD_TEST_PID", &pid_file);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"kept\x16");
    terminal.until("Reading clipboard");
    let deadline = Instant::now() + PATIENCE;
    while !pid_file.exists() {
        assert!(
            Instant::now() < deadline,
            "Clipboard reader was not started"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let pid = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    terminal.write(b"\n");
    terminal.until("Wait for clipboard insertion");
    terminal.raw(b"\x03");
    terminal.until("Ctrl+C: clear");
    let deadline = Instant::now() + PATIENCE;
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "Cancelled clipboard reader was not reaped"
        );
        thread::sleep(Duration::from_millis(10));
    }
    terminal.write(b"after cancel\n");
    terminal.editor();
    fs::remove_file(helper).unwrap();
    terminal.write(b"retained\x16");
    terminal.until("Cannot start wl-paste");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn clipboard_uri_lists_fall_back_to_text_and_reject_mixed_file_groups() {
    let home = Home::new();
    let bin = home.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let helper = bin.join("wl-paste");
    fs::write(
        &helper,
        r#"#!/bin/sh
if [ "$1" = "--list-types" ]; then
  printf 'text/uri-list\ntext/plain;charset=utf-8\n'
elif [ "$3" = "text/uri-list" ]; then
  /bin/cat "$DIM_CLIPBOARD_TEST_URIS"
else
  printf 'https://example.invalid/copied'
fi
"#,
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let uris = home.0.join("uris");
    fs::write(&uris, "# copied link\nhttps://example.invalid/copied\n").unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for text in ["https://example.invalid/copied", "kept"] {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], text);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":25}));
        }
        wire.detach();
    });
    let mut command = home.command();
    command
        .env("PATH", &bin)
        .env("WAYLAND_DISPLAY", "fixture")
        .env("DIM_CLIPBOARD_TEST_URIS", &uris);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"\x16");
    terminal.until("https://example.invalid/copied");
    terminal.write(b"\n");
    terminal.editor();
    fs::write(
        &uris,
        "file:///tmp/copied\nhttps://example.invalid/copied\n",
    )
    .unwrap();
    terminal.write(b"kept\x16");
    terminal.until("Clipboard file URL is not local");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_file_sources_preserve_order_bytes_and_the_local_resume_base() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    let local = home.0.join("local");
    let invoking = home.0.join("invoking");
    fs::create_dir_all(&local).unwrap();
    fs::create_dir_all(&invoking).unwrap();
    let png = image_fixture(image::ImageFormat::Png, 1, 1);
    let jpeg = image_fixture(image::ImageFormat::Jpeg, 1, 1);
    let first = local.join("wrong.txt");
    let second = local.join("other image.bin");
    let note = local.join("space note.md");
    fs::write(&first, &png).unwrap();
    fs::write(&second, &jpeg).unwrap();
    fs::write(&note, "P1 ordinary text reference contents stay local").unwrap();
    let expected_note = note.canonicalize().unwrap();
    let other = second.clone();
    let daemon = home.daemon(move |wire| {
        wire.start(true);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["user"]["content"], json!([
            {"type":"text","text":"Compare "},
            {"type":"image","source":{"type":"base64","mediaType":"image/png","data":STANDARD.encode(png)}},
            {"type":"text","text":expected_note.to_str().unwrap()},
            {"type":"image","source":{"type":"base64","mediaType":"image/jpeg","data":STANDARD.encode(jpeg)}},
            {"type":"text","text":" done"}
        ]));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":25}));
        for text in [expected_note.to_str().unwrap().to_owned(), "prefix wrong.txt suffix".into(), "https://example.invalid/image.png".into(), "file://remote/tmp/photo.png".into()] {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], text);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":25}));
        }
        assert!(other.exists());
        wire.detach();
    });
    let mut command = home.command();
    command
        .arg("--resume")
        .arg("session")
        .arg("--cwd")
        .arg(&local)
        .current_dir(&invoking);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"Compare ");
    terminal.paste("'wrong.txt' space\\ note.md");
    terminal.until("[File: space note.md]");
    fs::write(&first, "changed after capture").unwrap();
    terminal.paste(url::Url::from_file_path(&second).unwrap().as_str());
    terminal.until("[Image 2: other image.bin]");
    terminal.write(b" done\n");
    terminal.editor();
    fs::write(&note, "BM ordinary text is still a path reference").unwrap();
    terminal.paste("space note.md");
    terminal.until("[File: space note.md]");
    terminal.write(b"\n");
    terminal.editor();
    for text in [
        "prefix wrong.txt suffix",
        "https://example.invalid/image.png",
        "file://remote/tmp/photo.png",
    ] {
        terminal.paste(text);
        terminal.write(b"\n");
        terminal.editor();
    }
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("local files:"));
    assert!(first.exists() && note.exists() && second.exists());
}

#[test]
fn interactive_file_groups_reject_atomically_and_marker_removal_releases_capacity() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    fs::write(home.0.join("note.md"), "note").unwrap();
    let broken = format!("broken-{}.png", "x".repeat(100));
    fs::write(home.0.join(&broken), "not png").unwrap();
    let png = image_fixture(image::ImageFormat::Png, 1, 1);
    fs::write(home.0.join("one.png"), &png).unwrap();
    let gif = image_fixture(image::ImageFormat::Gif, 1, 1);
    fs::write(home.0.join("two.gif"), &gif).unwrap();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "kept");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 20);
        for part in &content[..19] {
            assert_eq!(part["source"]["data"], STANDARD.encode(&png));
        }
        assert_eq!(content[19]["source"]["mediaType"], "image/gif");
        assert_eq!(content[19]["source"]["data"], STANDARD.encode(gif));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut command = home.command();
    command.arg("--cwd").arg(&home.0);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"kept");
    terminal.paste(&format!("note.md {broken}"));
    terminal.until("PNG, JPEG, WebP or GIF");
    terminal.write(b"\n");
    terminal.editor();
    terminal.paste(&["one.png"; 20].join(" "));
    terminal.until("[Image 20: one.png]");
    terminal.paste("note.md two.gif");
    terminal.until("at most 20 images");
    terminal.raw(b"\x7f");
    terminal.paste("two.gif");
    terminal.until("[Image 20: two.gif]");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(home.0.join("one.png").exists());
    assert!(home.0.join("two.gif").exists());
}

#[test]
fn interactive_file_completion_navigates_selects_and_dismisses_without_submitting() {
    let home = Home::new();
    let dir = home.0.join("docs a");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("a.md"), "a").unwrap();
    fs::write(dir.join("b.md"), "b").unwrap();
    let hostile = "bad\n\u{1b}[31m.txt";
    fs::write(home.0.join(hostile), "safe").unwrap();
    let expected = [
        dir.join("b.md")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned(),
        "literal @docs".into(),
        "kept @missing/file".into(),
        home.0
            .join(hostile)
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned(),
        "delayed @docsfragmented\n/exit\n".into(),
    ];
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        for text in expected {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], text);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":25}));
        }
        wire.detach();
    });
    let mut command = home.command();
    command.arg("--cwd").arg(&home.0);
    let mut terminal = Terminal::launch(command);
    terminal.editor();
    terminal.write(b"@docs");
    terminal.until("* docs a/");
    terminal.raw(b"\t");
    terminal.until("* a.md");
    terminal.raw(b"\x1b[B");
    terminal.until("* b.md");
    terminal.raw(b"\t");
    terminal.until("[File: b.md]");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"literal @docs");
    terminal.until("Esc: close suggestions");
    terminal.raw(b"\x1b");
    terminal.until("Enter: send");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"kept @missing/file");
    terminal.until("No such file or directory");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"@bad");
    terminal.until("bad\\n\\u{1b}[31m.txt");
    terminal.raw(b"\t");
    terminal.until("[File: bad\\n\\u{1b}[31m.txt]");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"delayed @docs");
    terminal.until("Esc: close suggestions");
    terminal.raw(b"\x1b");
    terminal.until("Enter: send");
    terminal.raw(b"[200~fragmented\n/exit\n\x1b[201~");
    terminal.until("[Paste 1: 2 lines]");
    terminal.write(b"\n");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("\u{1b}[31m"));
}

/// The line composer edits graphemes atomically: inserting a combining
/// character can merge both adjacent graphemes, so the cursor snaps to a
/// grapheme boundary and the next delete or backspace removes the cluster.
#[test]
fn interactive_composer_preserves_multiline_pastes_and_edits_atomic_markers() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for expected in [
            "one\ntwo",
            "before start\r\n中\n/exit\nend\u{3}\n after",
            "remove keep [Paste 1: 2 lines]",
            "ABC",
            "first\nZsecond",
            "kept across EOF",
            "shift\nenter\nworks",
            "after malformed UTF-8",
            "standalone\rCR\r",
            "e\u{301}ok",
            "resized draft",
        ] {
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], expected);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":25}));
        }
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.raw(b"one\ntwo");
    terminal.until("two");
    terminal.raw(b"\r");
    terminal.until("\n> ");
    terminal.until("Ctrl+C: clear");
    terminal.raw(b"before ");
    terminal.until("before ");
    terminal.until("Ctrl+C: clear");
    terminal.raw(b"\x1b[20");
    terminal.raw("0~start\r\n中\n/exit\nend\u{3}\n\x1b[2".as_bytes());
    terminal.raw(b"01~");
    terminal.until("[Paste 1: 4 lines]");
    terminal.raw(b" after\r");
    terminal.until("\n> ");
    terminal.raw("remove \x1b[200~discard\nme\x1b[201~\x7fe\u{301}👩‍💻🇨🇳\x1b[D\x7f\x1b[C\x7f\x7fkeep [Paste 1: 2 lines]\r".as_bytes());
    terminal.until("\n> ");
    terminal
        .raw(b"A\x1b[200~x\n\x1b[201~B\x1b[200~y\n\x1b[201~C\x1b[H\x1b[C\x1b[3~\x1b[F\x1b[D\x7f\r");
    terminal.until("\n> ");
    terminal.raw(b"first\nsecond\x1b[H\x1b[A\x1b[BZ\x1b[F\r");
    terminal.until("\n> ");
    terminal.write(b"kept across EOF");
    terminal.raw(b"\x04");
    terminal.until("Draft retained; clear it before exiting");
    terminal.raw(b"\x1a\r");
    terminal.until("\n> ");
    terminal.raw(b"shift\x1b[13;2uenter\x1b[27;2;13~works\r");
    terminal.until("\n> ");
    terminal.raw(b"\xf0after malformed UTF-8\r");
    terminal.until("\n> ");
    terminal.paste("standalone\rCR\r");
    terminal.until("[Paste 1: 1 lines]");
    terminal.raw(b"\r");
    terminal.until("\n> ");
    terminal.raw(
        "e\u{301}\x1b[200~p\n\x1b[201~👩‍💻🇨🇳\x1b[D\x7f\x1b[D\x1b[3~\x1b[H\x1b[C\x1b[3~ok\r"
            .as_bytes(),
    );
    terminal.until("\n> ");
    let size = libc::winsize {
        ws_row: 6,
        ws_col: 15,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    terminal.signal(libc::SIGWINCH);
    terminal.raw(b"resized draft\r");
    terminal.until("\n> ");
    terminal.raw(b"\x04");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let mut mode = unsafe { std::mem::zeroed::<libc::termios>() };
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.master.as_ref().unwrap().as_raw_fd(), &mut mode) },
        0
    );
    assert_eq!(
        mode.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG),
        libc::ICANON | libc::ECHO | libc::ISIG,
    );
}

#[test]
fn interactive_output_survives_terminal_backpressure() {
    for interactive in [true, false] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12, "text", json!({"turn_id":"turn","request_id":"request","delta":format!("{}output complete", "x".repeat(512 * 1024))}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
        let mut command = home.command();
        if !interactive {
            command.args(["exec", "--prompt", "continue"]);
        }
        let mut terminal = Terminal::launch(command);
        if interactive {
            terminal.until("Ctrl+J: newline");
            terminal.write(b"continue\n");
        }
        thread::sleep(Duration::from_millis(150));
        if interactive {
            terminal.until("output complete");
            terminal.until("Ctrl+J: newline");
            terminal.write(b"/exit\n");
        }
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(terminal.transcript.contains(&"x".repeat(512 * 1024)));
        assert!(terminal.transcript.contains("output complete"));
    }
}

#[test]
#[cfg(target_os = "macos")]
fn interactive_input_names_its_terminal_without_listing_dev_while_terminals_close() {
    const SANDBOX: &str = "/usr/bin/sandbox-exec";
    const PROFILE: &str = r#"(version 1)(allow default)(deny file-read-data (literal "/dev"))"#;
    let listing = Command::new(SANDBOX)
        .args(["-p", PROFILE, "/bin/ls", "/dev"])
        .output()
        .unwrap();
    assert!(
        !listing.status.success(),
        "The profile must deny the /dev listing"
    );
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        wire.detach();
    });
    let (ready, started) = std::sync::mpsc::channel();
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let churn = thread::spawn(move || {
        let mut ready = Some(ready);
        loop {
            let (mut master, mut slave) = (0, 0);
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            unsafe {
                libc::close(slave);
                libc::close(master);
            }
            if let Some(ready) = ready.take() {
                ready.send(()).unwrap();
            }
            if stopped.try_recv() != Err(std::sync::mpsc::TryRecvError::Empty) {
                break;
            }
        }
    });
    started.recv().unwrap();
    let mut command = Command::new(SANDBOX);
    command
        .args([
            "-p",
            PROFILE,
            env!("CARGO_BIN_EXE_dimcode"),
            "--no-tui",
            "--home",
        ])
        .arg(&home.0);
    let mut terminal = Terminal::launch(command);
    terminal.until("Ctrl+J: newline");
    drop(stop);
    churn.join().unwrap();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_commands_reject_attached_drafts_before_storing_or_executing() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "  !literaldo not execute\n");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    for prefix in [
        "/exit ",
        "/export markdown file ",
        "!!echo ",
        "/mcp login docs ",
        "/effort high ",
        "/reasoning low ",
        "/fast on ",
        "/agent profile ",
    ] {
        terminal.write(prefix.as_bytes());
        terminal.paste(&"large text\n".repeat(2000));
        terminal.until("[Paste 1: 2000 lines]");
        terminal.write(b"\n");
        terminal.until("Commands require plain single-line input");
        terminal.raw(b"\x03");
    }
    terminal.write(b"  !literal");
    terminal.paste("do not execute\n");
    terminal.until("[Paste 1: 1 lines]");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_paste_put_rejection_retains_confirmed_paths_without_uploading_again() {
    let home = Home::new();
    let first = "first\r\n".repeat(3000);
    let second = "second\n".repeat(3000);
    let expected = first.clone();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let put = wire.read("attachment.put");
        assert_eq!(put["params"]["text"], expected);
        wire.reply(&put, json!({"attachmentId":"first","path":"/files/first.txt","bytes":expected.len()}));
        let put = wire.read("attachment.put");
        assert_eq!(put["params"]["text"], second);
        wire.send(json!({"jsonrpc":"2.0","id":put["id"],"error":{"code":-32000,"message":"storage failed","data":{"appError":"storage_failed"}}}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "[Pasted text: /files/first.txt (3000 lines)]");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":25}));
        let put = wire.read("attachment.put");
        wire.send(json!({"jsonrpc":"2.0","id":put["id"],"error":{"code":-32601,"message":"unknown_method","data":{"appError":"unknown_method"}}}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "clean draft");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.paste(&first);
    terminal.until("[Paste 1: 3000 lines]");
    terminal.paste(&"second\n".repeat(3000));
    terminal.until("[Paste 2: 3000 lines]");
    terminal.write(b"\n");
    terminal.until("Paste 2: storage failed");
    terminal.raw(b"\x7f\r");
    terminal.until("\n> ");
    terminal.paste(&first);
    terminal.until("[Paste 1: 3000 lines]");
    terminal.write(b"\n");
    terminal.until("unknown_method");
    terminal.raw(b"\x03");
    terminal.write(b"clean draft\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_paste_cancellation_drains_the_put_and_reuses_its_confirmed_path() {
    let home = Home::new();
    let text = "cancelled upload\r\n".repeat(1100);
    let (accepted, admitted) = std::sync::mpsc::channel();
    let (release, drain) = std::sync::mpsc::channel();
    let expected = text.clone();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let put = wire.read("attachment.put");
        assert_eq!(put["params"]["text"], expected);
        accepted.send(()).unwrap();
        drain.recv_timeout(PATIENCE).unwrap();
        wire.reply(
            &put,
            json!({"attachmentId":"drained","path":"/files/drained.txt","bytes":expected.len()}),
        );
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"],
            "[Pasted text: /files/drained.txt (1100 lines)]"
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.paste(&text);
    terminal.until("[Paste 1: 1100 lines]");
    terminal.write(b"\n");
    admitted.recv_timeout(PATIENCE).unwrap();
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for the text attachment to settle");
    terminal.signal(libc::SIGINT);
    terminal.until("Still waiting for the text attachment to settle");
    release.send(()).unwrap();
    terminal.until("Preparation cancelled");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_paste_encoding_rejection_keeps_confirmed_items_editable() {
    let home = Home::new();
    let path = format!("/files/{}.txt", "\"".repeat(4 * 1024 * 1024));
    let expected = path.clone();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        for _ in 0..2 {
            let put = wire.read("attachment.put");
            assert_eq!(put["params"]["text"], "x".repeat(16 * 1024 + 1));
            wire.reply(
                &put,
                json!({"attachmentId":"stored","path":expected,"bytes":16*1024+1}),
            );
        }
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"],
            format!("[Pasted text: {expected} (1 lines)]")
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    for index in [1, 2] {
        terminal.paste(&"x".repeat(16 * 1024 + 1));
        terminal.until(&format!("[Paste {index}: 1 lines]"));
    }
    terminal.write(b"\n");
    terminal.until("exceeds 16 MiB after JSON encoding");
    terminal.raw(b"\x7f\r");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_paste_boundaries_reject_without_losing_the_existing_draft() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"],
            format!("kept{}", "\0".repeat(16 * 1024))
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":25}));
        let put = wire.read("attachment.put");
        let text = put["params"]["text"].as_str().unwrap();
        assert_eq!(text.len(), 10 * 1024 * 1024);
        assert!(text.bytes().all(|byte| byte == b'x'));
        wire.reply(
            &put,
            json!({"attachmentId":"maximum","path":"/files/max.txt","bytes":text.len()}),
        );
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"],
            "[Pasted text: /files/max.txt (1 lines)]"
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "after");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"kept");
    terminal.raw(b"\x1b[200~unfinished\x03");
    terminal.raw(b"\x03");
    terminal.until("Paste cancelled; draft retained");
    terminal.raw(b"\x1b[200~\xff\x1b[201~");
    terminal.until("Paste must be UTF-8");
    terminal.paste(&"x".repeat(10 * 1024 * 1024 + 1));
    terminal.until("Paste exceeds 10 MiB");
    terminal.paste(&"\0".repeat(16 * 1024));
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.paste(&"x".repeat(10 * 1024 * 1024));
    terminal.until("[Paste 1: 1 lines]");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.paste(&"\0".repeat(3 * 1024 * 1024));
    terminal.until("[Paste 1: 1 lines]");
    terminal.write(b"\n");
    terminal.until("exceeds 16 MiB after JSON encoding");
    terminal.raw(b"\x7f");
    terminal.write(b"after\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains('\0'));
    assert!(terminal.transcript.contains("\\0"));
}

#[test]
fn interactive_put_uncertainty_exits_without_another_put_or_submission() {
    for disconnect in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let put = wire.read("attachment.put");
            if !disconnect {
                wire.reply(
                    &put,
                    json!({"path":123,"attachmentId":"stored","bytes":20000}),
                );
                wire.detach();
            }
        });
        let mut terminal = Terminal::launch(home.command());
        terminal.until("\n> ");
        terminal.paste(&"x".repeat(20000));
        terminal.until("[Paste 1: 1 lines]");
        terminal.write(b"\n");
        terminal.until(if disconnect {
            "App Server connection closed"
        } else {
            "Attachment outcome could not be confirmed"
        });
        assert_eq!(terminal.wait().code(), Some(1));
        daemon.join().unwrap();
    }
}

#[test]
fn interactive_shell_prefixes_use_session_methods_and_keep_failures_out_of_model_input() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for _ in 0..2 { let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":8})); }
        let mut identities = std::collections::HashSet::new();
        for (mode, command) in [("record", " printf 'recorded' "), ("private", "printf private"), ("record", "broken"), ("record", "busy"), ("record", "unknown"), ("record", "opaque"), ("private", "!nested")] {
            let run = wire.read("shell.run");
            assert_eq!(run["params"]["sessionId"], "session");
            assert_eq!(run["params"]["branchId"], "main");
            assert_eq!(run["params"]["mode"], mode);
            assert_eq!(run["params"]["command"], command);
            let id = run["params"]["runId"].as_str().unwrap();
            assert!(uuid::Uuid::parse_str(id).is_ok());
            assert!(identities.insert(id.to_owned()));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":8}));
            if command == "unknown" || command == "opaque" {
                wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32601,"message":"Unavailable","data":if command == "unknown" {json!({"details":"opaque-private"})} else {json!("opaque-private")}}}));
                continue;
            }
            if command == "busy" {
                wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32000,"message":"Already running","data":{"appError":"shell_busy"}}}));
                continue;
            }
            let mut reply = json!({"runId":id,"status":"completed","exitCode":0,"output":"final-\u{1b}[31mresult\u{1b}[0m\u{202e}","truncated":false,"durationMs":1});
            if mode == "record" {
                reply["recorded"] = json!({"sequence":9});
            }
            if command == "broken" {
                reply["status"] = "failed".into();
                reply["reason"] = "read failed".into();
                reply["truncated"] = true.into();
                reply["recorded"] = json!({"error":{"appError":"branch_closed","message":"Branch closed"}});
            }
            wire.reply(&run, reply);
        }
        let submit=wire.read("session.submit"); assert_eq!(submit["params"]["text"],"  !literal"); wire.reply(&submit,json!({"event":{}}));
        let idle=wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"!\n");
    terminal.until("Usage: !command");
    terminal.until("\n> ");
    terminal.write(b"!!\n");
    terminal.until("Usage: !command");
    terminal.until("\n> ");
    terminal.write(b"! printf 'recorded' \n");
    terminal.until("[shell recorded] sequence: 9");
    terminal.until("\n> ");
    terminal.write(b"!!printf private\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"!broken\n");
    terminal.until("[shell recording failed] Branch closed (branch_closed)");
    terminal.until("\n> ");
    terminal.write(b"!busy\n");
    terminal.until("[shell error] Already running (RPC -32000, shell_busy)");
    terminal.until("\n> ");
    for command in ["!unknown", "!opaque"] {
        terminal.write(format!("{command}\n").as_bytes());
        terminal.until("[shell error] Unavailable (RPC -32601)");
        terminal.until("\n> ");
    }
    terminal.write(b"!!!nested\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"  !literal\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("final-result"));
    assert!(terminal.transcript.contains("(output truncated)"));
    assert_eq!(terminal.transcript.matches("[shell recorded]").count(), 1);
    assert!(!terminal.transcript.contains("opaque-private"));
    assert!(!terminal.transcript.contains("\x1b[31m"));
    assert!(!terminal.transcript.contains('\u{202e}'));
}

#[test]
fn interactive_shell_runs_during_a_turn_preserve_approval_focus_and_two_stage_interrupts() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "work");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","textOffset":0,"text":"model working","thinking":"","thinkingOffset":0}));
        let run = wire.read("shell.run");
        assert_eq!(run["params"]["mode"], "record");
        assert_eq!(run["params"]["command"], "inspect");
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":null}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"li\u{1b}[31mve","skipped":7}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":" out\u{202e}put"}}));
        wire.notice("management.progress", json!({"id":99999,"method":"shell.run","data":{"text":"foreign progress"}}));
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"command":"inspect"}},"reason":"review"}));
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":"authoritative output","truncated":false,"durationMs":1,"recorded":{"sequence":12}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"late progress must be ignored"}}));
        let answer = wire.read("approval.respond");
        assert_eq!(answer["params"]["decision"], "deny");
        wire.reply(&answer, json!({}));
        let run = wire.read("shell.run");
        assert_eq!(run["params"]["mode"], "private");
        assert_eq!(run["params"]["command"], "wait");
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"waiting"}}));
        let shell_stop = wire.read("shell.stop");
        assert_eq!(shell_stop["params"]["runId"], run["params"]["runId"]);
        let stop = wire.read("session.stop");
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false,"durationMs":0}));
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&stop, json!({"sequence":40}));
        wire.reply(&idle, json!({"sequence":40}));
        wire.reply(&shell_stop, json!({"stopped":false}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "next");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":40}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"work\n");
    terminal.until("model working");
    terminal.write(b"ordinary typeahead\n");
    terminal.until("[busy] Wait for the prompt");
    terminal.write(b"!inspect\n");
    terminal.until("Allow once? [y/N] ");
    assert!(!terminal.transcript.contains("authoritative output"));
    terminal.write(b"!never-execute-this-answer\n");
    terminal.until("[shell recorded] sequence: 12");
    terminal.write(b"!!wait\n");
    terminal.until("waiting");
    terminal.write(b"!another\n");
    terminal.until("[shell busy]");
    terminal.signal(libc::SIGINT);
    terminal.until("[shell stopping] Waiting for command settlement");
    terminal.signal(libc::SIGINT);
    terminal.until("[stopping] Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(b"next\n");
    terminal.until("\n> ");
    terminal.write(&[4]);
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("skipped 7 bytes"));
    assert!(!terminal.transcript.contains("foreign progress"));
    assert_eq!(terminal.transcript.matches("[shell started]").count(), 2);
    assert_eq!(
        terminal
            .transcript
            .matches("[shell progress] (incomplete)")
            .count(),
        2
    );
    assert!(terminal.transcript.contains("live output"));
    assert!(terminal.transcript.contains("[shell progress error]"));
    assert!(
        !terminal
            .transcript
            .contains("late progress must be ignored")
    );
    assert!(!terminal.transcript.contains("\x1b[31m"));
    assert!(!terminal.transcript.contains('\u{202e}'));
}

#[test]
fn interactive_shell_eof_stops_an_unanswered_run_and_detaches_after_both_replies() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let run = wire.read("shell.run");
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":8}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"before admission"}}));
        let stop = wire.read("shell.stop");
        assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false,"durationMs":0}));
        wire.reply(&stop, json!({"stopped":false}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"!wait\n");
    terminal.until("before admission");
    terminal.write(&[4]);
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interactive_default_deny_discards_typeahead_and_ctrl_c_settles_before_next_input() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"first-request","textOffset":0,"text":"working","thinking":"","thinkingOffset":0}));
        thread::sleep(Duration::from_millis(150));
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"description":"Inspect workspace","command":"printf workspace-inspected","cwd":"/tmp/approval-fixture","secret":"never print me"}},"reason":"shell command"}));
        let answer = wire.read("approval.respond");
        assert_eq!(answer["params"]["decision"],"deny");
        wire.reply(&answer,json!({}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"denied"}));
        wire.reply(&idle,json!({"sequence":30}));
        let submit = wire.read("session.submit"); assert_eq!(submit["params"]["text"],"second");
        wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"second-request","textOffset":0,"text":"partial","thinking":"","thinkingOffset":0}));
        let ended = json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":35,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"second-turn","status":"cancelled","reason":"interrupted"}}}}).to_string();
        wire.0.get_mut().write_all(&ended.as_bytes()[..10]).unwrap();
        wire.0.get_mut().flush().unwrap();
        let stop = wire.read("session.stop");
        writeln!(wire.0.get_mut(),"{}",&ended[10..]).unwrap();
        wire.reply(&stop,json!({"sequence":40}));
        thread::sleep(Duration::from_millis(100));
        wire.reply(&idle,json!({"sequence":40}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"first\n");
    terminal.until("working");
    terminal.write(b"yes\n");
    terminal.until("Allow once? [y/N] ");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.write(b"second\n");
    terminal.until("partial");
    thread::sleep(Duration::from_millis(100));
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(&[4]);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success());
    assert!(
        terminal
            .transcript
            .contains("command: printf workspace-inspected"),
        "{}",
        terminal.transcript
    );
    assert!(
        terminal.transcript.contains("cwd: /tmp/approval-fixture"),
        "{}",
        terminal.transcript
    );
    assert!(!terminal.transcript.contains("never print me"));
}

#[test]
fn interactive_configuration_commands_route_and_recover_without_submitting_commands() {
    fn history(wire: &mut Wire) {
        let request = wire.read("session.history");
        let events = if request["params"]["after"].as_u64().unwrap() < 9 {
            json!([{"sequence":9,"create_at":1,"type":"context","name":"configured","data":{
                "timeline":"main","revision":"configured","previous":null,"prompt":null,
                "provider_id":"test","model_id":"model"}}])
        } else {
            json!([])
        };
        wire.reply(
            &request,
            json!({"events":events,"through":9,"nextAfter":9,"done":true}),
        );
    }
    fn catalog(wire: &mut Wire) {
        let request = wire.read("provider.catalog");
        assert_eq!(request["params"], json!({"providerId":"test"}));
        let mut capabilities =
            serde_json::to_value(dim_agent_types::ModelCapabilities::default()).unwrap();
        capabilities["reasoning"] = json!(true);
        wire.reply(
            &request,
            json!({"catalog":{"models":[{"id":"model","capabilities":capabilities,
            "metadata":{"reasoning":{"supported":true,"effortOptions":["high","low"]},
                "serviceTier":{"supported":true,"options":["priority","default"]}}}]}}),
        );
    }
    let home = Home::new();
    let exported = home.0.join("configured.json");
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        history(wire);
        catalog(wire);
        history(wire);
        catalog(wire);
        let effort = wire.read("session.model.update");
        assert_eq!(effort["params"], json!({"sessionId":"session","branchId":"main","modelPreferences":{"reasoningEffort":"high"}}));
        entered.send(()).unwrap();
        wait_release.recv_timeout(PATIENCE).unwrap();
        wire.reply(&effort, json!({"outcome":"adopted","model":{"providerId":"test","modelId":"model"}}));
        history(wire);
        history(wire);
        history(wire);
        catalog(wire);
        let fast = wire.read("session.model.update");
        assert_eq!(fast["params"]["modelPreferences"], json!({"serviceTier":"default"}));
        wire.send(json!({"jsonrpc":"2.0","id":fast["id"],"error":{"code":-32000,"message":"fixture preference rejection"}}));
        let agents = wire.read("agent.list");
        wire.reply(&agents, json!([{"agentId":"profile-id","name":"Reviewer","description":"Review changes"}]));
        let agent = wire.read("session.agent.update");
        assert_eq!(agent["params"], json!({"sessionId":"session","branchId":"main","agentId":"profile-id"}));
        wire.reply(&agent, json!({"outcome":"adopted"}));
        history(wire);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "after configuration");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":9}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"/effort\n");
    terminal.until("Reasoning effort choices: high, low");
    terminal.until("\n> ");
    terminal.write(b"/reasoning high\n");
    wait_entered.recv_timeout(PATIENCE).unwrap();
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for configuration result");
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for configuration result");
    release.send(()).unwrap();
    terminal.until("Reasoning effort configuration adopted: high");
    terminal.until("\n> ");
    terminal.write(format!("/export json {}\n", exported.display()).as_bytes());
    terminal.until("Exported JSON:");
    terminal.until("\n> ");
    terminal.write(b"/fast off\n");
    terminal.until("Error: fixture preference rejection");
    terminal.until("\n> ");
    terminal.write(b"/agent\n");
    terminal.until("profile-id\tReviewer\tReview changes");
    terminal.until("\n> ");
    terminal.write(b"/agent profile-id\n");
    terminal.until("Session tools unchanged.");
    terminal.until("\n> ");
    terminal.write(b"after configuration\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let data: Value = serde_json::from_slice(&fs::read(exported).unwrap()).unwrap();
    assert_eq!(data["through"], 9);
    assert_eq!(data["events"][0]["data"]["model_id"], "model");
}

#[test]
fn explicit_model_selection_uses_protocol_field_names_from_tui_and_line_client() {
    for tui in [true, false] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list","provider.management","ui.leaf"]}));
            let management = wire.read("provider.management");
            assert_eq!(management["params"], json!({"providerId":"fixture"}));
            wire.reply(&management, json!({"providers":[{"providerId":"fixture","source":"custom","displayName":"Fixture","enabled":true,"version":1,"connection":null,"preferences":{"fixture-model":{"reasoningEffort":"high","serviceTier":"priority"}}}],"errors":[]}));
            let create = wire.read("session.create");
            assert_eq!(create["params"]["model"], json!({"provider":"fixture","modelId":"fixture-model"}));
            assert_eq!(create["params"]["modelPreferences"], json!({"reasoningEffort":"high","serviceTier":"priority"}));
            wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,"message":"fixture stops here","data":{"appError":"provider_failed"}}}));
        });
        let mut command = home.command();
        command.args(["--provider", "fixture", "--model", "fixture-model"]);
        if tui {
            let mut terminal = Terminal::launch_tui(command);
            terminal.until("fixture stops here");
            assert!(!terminal.wait().success());
            daemon.join().unwrap();
        } else {
            command
                .args(["--prompt", "hello"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let output = wait(command.spawn().unwrap());
            daemon.join().unwrap();
            assert!(!output.status.success());
        }
    }
}

#[test]
fn fullscreen_session_attached_before_the_first_frame_shows_one_banner() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
    assert_eq!(
        terminal.transcript.matches("Welcome to DIM").count(),
        1,
        "{}",
        terminal.transcript
    );
}

#[test]
fn tui_shell_mode_runs_through_session_methods_and_shows_the_recorded_run() {
    let home = Home::new();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let run = wire.read("shell.run");
        let params = &run["params"];
        assert_eq!((&params["sessionId"], &params["branchId"]), (&json!("session"), &json!("main")));
        assert_eq!((&params["command"], &params["mode"]), (&json!("echo recorded"), &json!("record")));
        let run_id = params["runId"].clone();
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"live-out\u{1b}]52;c;blocked-clipboard\u{7}"}}));
        wait_release.recv_timeout(PATIENCE).unwrap();
        let marker = "<dim-shell-input kind=\"command\" status=\"completed\" exit=\"0\" truncated=\"false\">\n$ echo recorded\nrecorded-out\u{1b}]0;blocked-title\u{7}\u{1b}]8;;file:///tmp/blocked-link\u{7}hidden\u{1b}]8;;\u{7}\n\n</dim-shell-input>";
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"shell","sequence":9,"create_at":1,"type":"input","name":"message","data":{"blocks":[{"type":"raw","text":marker}],"mode":"append"}}}));
        wire.reply(&run, json!({"runId":run_id,"status":"completed","exitCode":0,"output":"recorded-out\n","truncated":false,"durationMs":1,"recorded":{"sequence":9}}));
        let run = wire.read("shell.run");
        assert_eq!((&run["params"]["command"], &run["params"]["mode"]), (&json!("echo private"), &json!("private")));
        let stop = wire.read("shell.stop");
        assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
        wire.reply(&stop, json!({"stopped":true}));
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"private-out\u{1b}]52;c;blocked-private-clipboard\u{7}\n","truncated":false,"durationMs":1}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"!echo recorded\r");
    terminal.until("live-out");
    terminal.write(b"!echo rejected\r");
    terminal.until("A shell command is already running");
    release.send(()).unwrap();
    terminal.until("recorded-out");
    terminal.write(b"\x15");
    terminal.write(b"!echo private\r");
    terminal.until("Ctrl+C to interrupt");
    terminal.write(b"\x03");
    terminal.until("private-out");
    terminal.until("not sent to the model");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
    for payload in [
        "blocked-title",
        "blocked-clipboard",
        "blocked-private-clipboard",
        "file:///tmp/blocked-link",
    ] {
        assert!(
            !terminal.transcript.contains(payload),
            "{payload}: {}",
            terminal.transcript
        );
    }
    assert!(
        !terminal.transcript.contains("<dim-shell-input"),
        "{}",
        terminal.transcript
    );
}

#[test]
fn tui_mcp_and_configuration_commands_use_session_methods() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.notice("session.mcp.state", json!({"sessionId":"session","servers":[{"id":"docs","state":"auth_required"}]}));
        let server = json!({"id":"docs","source":"user","enabled":true,"transport":"http","auth":{"status":"required"}});
        let catalog = json!({"userRevision":"1","projectRevision":null,"servers":[server],"userApplication":{},"projectApplication":null});
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog.clone());
        let state = wire.read("session.mcp");
        assert_eq!(state["params"], json!({"sessionId":"session"}));
        wire.reply(&state, json!({"sessionId":"session","servers":[{"id":"docs","state":"auth_required"}]}));
        let request = wire.read("mcp.catalog");
        wire.reply(&request, catalog.clone());
        let start = wire.read("mcp.auth.start");
        assert_eq!((&start["params"]["id"], &start["params"]["source"]), (&json!("docs"), &json!("user")));
        let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 60_000;
        wire.reply(&start, json!({"attemptId":"attempt","authorizationUrl":"https://auth.example/authorize","redirectUri":"http://localhost:1/callback","resource":"https://docs.example/mcp","issuer":"https://auth.example","expiresAt":expires}));
        let complete = wire.read("mcp.auth.complete");
        assert_eq!(complete["params"], json!({"attemptId":"attempt","callbackUrl":"http://localhost:1/callback?code=c"}));
        wire.reply(&complete, json!({}));
        let request = wire.read("provider.catalog");
        assert_eq!(request["params"], json!({"providerId":"test"}));
        let mut capabilities = serde_json::to_value(dim_agent_types::ModelCapabilities::default()).unwrap();
        capabilities["reasoning"] = json!(true);
        wire.reply(&request, json!({"catalog":{"models":[{"id":"model","capabilities":capabilities,
            "metadata":{"reasoning":{"supported":true,"effortOptions":["high","low"]}}}]}}));
        let effort = wire.read("session.model.update");
        assert_eq!(effort["params"], json!({"sessionId":"session","branchId":"main","modelPreferences":{"reasoningEffort":"high"}}));
        wire.reply(&effort, json!({"outcome":"adopted"}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.env("SSH_TTY", "/dev/fixture");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("sign-in required");
    terminal.write(b"/mcp list\r");
    terminal.until("Columns: ID, source");
    terminal.write(b"/mcp login docs\r");
    terminal.until("auth.example/authorize");
    terminal.write(b"/mcp callback http://localhost:1/callback?code=c\r");
    terminal.until("Signed in to \"docs\"");
    terminal.write(b"/effort high\r");
    terminal.until("Reasoning effort configuration adopted: high");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_pasted_images_and_long_text_submit_as_user_content() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let home = Home::new();
    let image = home.0.join("shot.png");
    let bytes = image_fixture(image::ImageFormat::Png, 3, 2);
    fs::write(&image, &bytes).unwrap();
    let long = "y".repeat(16 * 1024 + 1);
    let expected_long = long.clone();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let put = wire.read("attachment.put");
        assert_eq!(put["params"], json!({"sessionId":"session","text":expected_long}));
        wire.reply(&put, json!({"attachmentId":"stored","path":"/files/paste.txt","bytes":16*1024+1}));
        let submit = wire.read("session.submit");
        assert!(submit["params"].get("text").is_none());
        assert_eq!(submit["params"]["user"]["content"], json!([
            {"type":"text","text":"look "},
            {"type":"image","source":{"type":"base64","mediaType":"image/png","data":STANDARD.encode(&bytes)}},
            {"type":"text","text":" [Pasted text: /files/paste.txt (1 lines)]"},
        ]));
        wire.reply(&submit, json!({"event":{}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"look ");
    terminal.paste(image.to_str().unwrap());
    terminal.until("[shot.png]");
    terminal.paste(&long);
    terminal.until("16385 chars");
    terminal.write(b"\r");
    let joined = daemon.join();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    joined.unwrap();
}

#[test]
fn tui_at_mentions_pick_files_from_the_working_directory() {
    let home = Home::new();
    let project = home.0.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("notes.txt"), "").unwrap();
    fs::write(project.join("src/main.rs"), "").unwrap();
    let resolved = project.canonicalize().unwrap();
    let expected = format!(
        "read [@notes.txt]({}) [@src/main.rs]({})",
        resolved.join("notes.txt").display(),
        resolved.join("src/main.rs").display()
    );
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], expected);
        wire.reply(&submit, json!({"event":{}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.args(["--cwd", project.to_str().unwrap()]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"read @no");
    terminal.until("Files");
    terminal.until("@notes.txt");
    terminal.write(b"\r");
    terminal.write(b"@sr");
    terminal.until("@src/");
    terminal.write(b"\t");
    terminal.until("@src/main.rs");
    terminal.write(b"\t");
    terminal.write(b"\r");
    let joined = daemon.join();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    joined.unwrap();
}

#[test]
fn tui_sends_dropped_files_as_links_and_images_by_model_capability() {
    let home = Home::new();
    let image = home.0.join("shot.png");
    fs::write(&image, image_fixture(image::ImageFormat::Png, 3, 2)).unwrap();
    let notes = home.0.join("my notes.txt");
    fs::write(&notes, "plain").unwrap();
    let expected = format!(
        "look [shot.png]({}) [my notes.txt](<{}>)",
        image.display(),
        notes.display()
    );
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list","provider.catalog"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"test"}));
        let capabilities = json!({"modalities":["text"],"toolCalling":true,"streaming":true,"structuredOutput":true,"vision":false,"audio":false,"reasoning":false,"contextWindow":128000,"maxOutputTokens":16384});
        wire.reply(&catalog, json!({"catalog":{"models":[{"id":"model","capabilities":capabilities}]}}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], expected);
        assert!(submit["params"].get("user").is_none());
        wire.reply(&submit, json!({"event":{}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"look ");
    let dragged = format!(
        "{} {}",
        image.display(),
        notes.display().to_string().replace(' ', "\\ ")
    );
    terminal.paste(&dragged);
    terminal.until("[my notes.txt]");
    terminal.write(b"\r");
    let joined = daemon.join();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    joined.unwrap();
}

#[test]
fn browser_login_hides_callback_and_uses_only_generic_account_rpc() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let providers = wire.read("account.providers");
        wire.reply(&providers,json!([{"providerId":"fixture","displayName":"Fixture","loginMethods":["browser"]}]));
        let begin = wire.read("account.login.browser.begin");
        assert_eq!(begin["params"]["directory"],"https://fixture.invalid");
        wire.reply(&begin,json!({"attemptId":"attempt","authorizationUrl":"urn:fixture:authorization","expiresAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() + 60_000}));
        let complete = wire.read("account.login.browser.complete");
        assert_eq!(complete["params"]["callbackUrl"],"http://localhost:1455/auth/callback?code=secret-code&state=state");
        wire.reply(&complete,json!({"credentialEpoch":"epoch","generation":1,"account":{"issuer":"fixture","subject":"test","grantedScopes":[]}}));
    });
    let mut command = home.command();
    command.args(["login", "fixture", "--directory", "https://fixture.invalid"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("Callback URL: ");
    terminal.write(b"http://localhost:1455/auth/callback?code=secret-code&state=state\n");
    terminal.until("Signed in.");
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success());
    assert!(
        !terminal.transcript.contains("secret-code"),
        "{}",
        terminal.transcript
    );
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe {
            libc::tcgetattr(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                attributes.as_mut_ptr(),
            )
        },
        0
    );
    assert_ne!(unsafe { attributes.assume_init() }.c_lflag & libc::ECHO, 0);
}

#[test]
fn device_login_polls_and_cancels_the_attempt_on_ctrl_c() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let providers = wire.read("account.providers");
        wire.reply(&providers,json!([{"providerId":"fixture","displayName":"Fixture","loginMethods":["device"]}]));
        let begin = wire.read("account.login.begin");
        wire.reply(&begin,json!({"attemptId":"device","verificationUri":"https://fixture.invalid/device","userCode":"TEST-CODE","expiresAt":999999}));
        let status = wire.read("account.login.status"); wire.reply(&status,json!({"status":"pending"}));
        let cancel = wire.read("account.login.cancel"); assert_eq!(cancel["params"]["attemptId"],"device"); wire.reply(&cancel,json!({}));
    });
    let mut command = home.command();
    command.args([
        "login",
        "fixture",
        "--directory",
        "https://fixture.invalid",
        "--device",
    ]);
    let mut terminal = Terminal::launch(command);
    terminal.until("TEST-CODE");
    thread::sleep(Duration::from_millis(100));
    terminal.signal(libc::SIGINT);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(130));
}

#[test]
#[ignore = "requires DIM_TEST_NPM_WRAPPER pointing to npm/bin/dimcode.mjs"]
fn unix_npm_wrapper_preserves_the_native_terminal_and_interrupt_flow() {
    let source =
        PathBuf::from(std::env::var("DIM_TEST_NPM_WRAPPER").expect("DIM_TEST_NPM_WRAPPER"));
    let home = Home::new();
    let node_platform = Command::new("node")
        .args(["-p", "process.platform+'-'+process.arch"])
        .output()
        .unwrap();
    assert!(node_platform.status.success());
    let platform = String::from_utf8(node_platform.stdout).unwrap();
    let package = home
        .0
        .join(format!("wrapper/node_modules/dimcode-{}", platform.trim()));
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"dimcode-{}\"}}", platform.trim()),
    )
    .unwrap();
    fs::copy(env!("CARGO_BIN_EXE_dimcode"), package.join("bin/dimcode")).unwrap();
    let wrapper = home.0.join("wrapper");
    let launcher = wrapper.join("bin").join("dimcode.mjs");
    fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    fs::copy(&source, &launcher).unwrap();
    fs::copy(
        source
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("package.json"),
        wrapper.join("package.json"),
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"streaming","thinking":"","thinkingOffset":0}));
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        thread::sleep(Duration::from_millis(100));
        wire.reply(&stop,json!({"sequence":30})); wire.reply(&idle,json!({"sequence":30}));
        wire.detach();
    });
    let mut command = Command::new("node");
    command
        .arg(launcher)
        .arg("--no-tui")
        .arg("--home")
        .arg(&home.0)
        .env("DIMCODE_UPDATE_CHECK", "0");
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"hello\n");
    terminal.until("streaming");
    assert_eq!(
        unsafe { libc::kill(-(terminal.child.id() as i32), libc::SIGINT) },
        0
    );
    terminal.until("Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(&[4]);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
}

struct OwnedDaemon(PathBuf);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let Ok(stream) = UnixStream::connect(&self.0) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let mut stream = BufReader::new(stream);
        for (id, method, params) in [
            (1, "hello", json!({"protocolVersion":3,"clientKind":"cli"})),
            (2, "daemon.shutdown", json!({})),
        ] {
            if writeln!(
                stream.get_mut(),
                "{}",
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
            )
            .is_err()
            {
                return;
            }
            loop {
                let mut line = String::new();
                if stream
                    .read_line(&mut line)
                    .ok()
                    .filter(|size| *size > 0)
                    .is_none()
                {
                    return;
                }
                if serde_json::from_str::<Value>(&line)
                    .ok()
                    .is_some_and(|reply| reply["id"] == id)
                {
                    break;
                }
            }
        }
    }
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn project_mcp_entries_start_disabled_and_the_user_enables_them() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    fs::write(project.join(".mcp.json"), json!({"mcpServers":{"docs":{"command":"missing-mcp-program","args":["original"],"env":{"SECRET":"must-not-print"},"enabled":true}}}).to_string()).unwrap();
    let run = |args: &[&str]| {
        home.command()
            .arg("--server")
            .arg(&binary)
            .arg("--model-config")
            .arg(&config)
            .arg("--cwd")
            .arg(&project)
            .args(args)
            .output()
            .unwrap()
    };
    let list = || {
        let output = run(&["mcp", "list"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(!output.contains("must-not-print"));
        output
    };
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
    let written = fs::read(project.join(".mcp.json")).unwrap();
    let enabled = run(&["mcp", "enable", "docs"]);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert_eq!(
        String::from_utf8(enabled.stdout).unwrap(),
        "Enabled \"docs\" (project).\n"
    );
    assert!(String::from_utf8_lossy(&enabled.stderr).contains(".mcp.json"));
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tenabled\t—\n");
    assert_eq!(fs::read(project.join(".mcp.json")).unwrap(), written);
    assert!(run(&["daemon", "stop"]).status.success());
    assert_eq!(
        list(),
        "\"docs\"\tproject\tstdio\tenabled\t—\n",
        "restart lost the choice"
    );
    assert!(run(&["mcp", "disable", "docs"]).status.success());
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
    assert!(run(&["mcp", "enable", "docs"]).status.success());
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{}}).to_string(),
    )
    .unwrap();
    let unlisted = run(&["mcp", "disable", "docs"]);
    assert!(unlisted.status.success());
    assert!(String::from_utf8_lossy(&unlisted.stderr).contains("\"docs\" is not in mcp list"));
    fs::write(project.join(".mcp.json"), &written).unwrap();
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
    assert!(run(&["mcp", "enable", "docs"]).status.success());
    fs::write(project.join(".mcp.json"), "{broken").unwrap();
    assert!(!run(&["mcp", "list"]).status.success());
    let unreadable = run(&["mcp", "disable", "docs"]);
    assert!(
        unreadable.status.success(),
        "{}",
        String::from_utf8_lossy(&unreadable.stderr)
    );
    assert_eq!(
        String::from_utf8(unreadable.stdout).unwrap(),
        "Disabled \"docs\" (project).\n"
    );
    assert!(String::from_utf8_lossy(&unreadable.stderr).contains("could not be read"));
    fs::write(project.join(".mcp.json"), &written).unwrap();
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{"remote":{"type":"http","url":"not a url"}}}).to_string(),
    )
    .unwrap();
    assert!(run(&["mcp", "enable", "remote"]).status.success());
    let reported = (0..50).any(|_| {
        let output = run(&["mcp", "list"]);
        let reported = String::from_utf8_lossy(&output.stderr)
            .contains("project MCP configuration: MCP configuration is not active");
        if !reported {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        reported
    });
    assert!(
        reported,
        "mcp list did not report the inactive configuration"
    );
    assert!(run(&["mcp", "disable", "remote"]).status.success());
    fs::write(project.join(".mcp.json"), &written).unwrap();
    let missing = run(&["mcp", "enable", "absent"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("No MCP server \"absent\""));
    for removed in [
        &["mcp", "approve", "docs", "token"][..],
        &["mcp", "revoke", "docs"],
    ] {
        let output = run(removed);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("mcp enable ID"));
    }
    let id = "untrusted\u{9b}2J\u{202e}\nname";
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{(id):{"command":"missing-mcp-program"}}}).to_string(),
    )
    .unwrap();
    let escaped = list();
    assert!(!escaped.contains(['\u{9b}', '\u{202e}']));
    assert_eq!(escaped.lines().count(), 1);
    assert!(escaped.contains("untrusted\\u{9b}2J\\u{202e}\\nname"));
    assert!(run(&["daemon", "stop"]).status.success());
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn real_daemon_profile_conflicts_preserve_the_running_daemon_and_endpoint() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    for mock in [true, false] {
        let home = Home::new();
        let original = home.0.join("original.json");
        let requested = home.0.join("requested.json");
        let body = json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1","models":[{"id":"fixture-model","config":{"maxTokens":128}}]}]}).to_string();
        fs::write(&original, &body).unwrap();
        fs::write(&requested, format!("{body}\n")).unwrap();
        let socket = home.0.join("rust/daemon.sock");
        let mut command = Command::new(&binary);
        command
            .arg("--unix")
            .arg(&socket)
            .arg("--home")
            .arg(home.0.join("rust"))
            .arg("--no-ws")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if mock {
            command.arg("--mock-model");
        } else {
            command.arg("--model-config").arg(&original);
        }
        let mut daemon = command.spawn().unwrap();
        let owned = OwnedDaemon(socket.clone());
        let deadline = Instant::now() + PATIENCE;
        while !socket.exists() {
            assert!(daemon.try_wait().unwrap().is_none(), "Daemon did not start");
            assert!(Instant::now() < deadline, "Daemon did not become ready");
            thread::sleep(Duration::from_millis(10));
        }
        let output = home
            .command()
            .args([
                "--server",
                &binary,
                "--model-config",
                requested.to_str().unwrap(),
                "--prompt",
                "must not run",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("different model profile"));
        assert!(output.stdout.is_empty());
        assert!(socket.exists());
        assert!(daemon.try_wait().unwrap().is_none());
        fs::write(&requested, "invalid changed configuration").unwrap();
        let stopped = home
            .command()
            .args([
                "--server",
                &binary,
                "--model-config",
                requested.to_str().unwrap(),
                "daemon",
                "stop",
            ])
            .output()
            .unwrap();
        assert!(
            stopped.status.success(),
            "{}",
            String::from_utf8_lossy(&stopped.stderr)
        );
        assert!(!socket.exists());
        assert!(home.0.join("rust").exists());
        drop(owned);
        assert!(wait(daemon).status.success());
    }
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn configured_api_key_model_uses_production_tools_and_restores_the_same_session() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let binary = std::env::var("DIM_TEST_APP_SERVER")
        .expect("Set DIM_TEST_APP_SERVER to the dim-app-server binary");
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let image = home.0.join("screenshot.dat");
    let bytes = image_fixture(image::ImageFormat::Png, 1, 1);
    fs::write(&image, &bytes).unwrap();
    let image_data = STANDARD.encode(bytes);
    let model_image = image_data.clone();
    let outside = Home::new();
    let reference = outside.0.join("outside reference.txt");
    fs::write(&reference, "outside-attachment-marker\n").unwrap();
    let model_reference = reference.clone();
    let paste = format!(
        "native-paste-reader-marker\r\n{}",
        "中 e\u{301} text\r\n".repeat(2000)
    );
    let model_paste = paste.clone();
    fs::write(home.0.join("marker.txt"), "production-read-marker\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let model = thread::spawn(move || {
        for phase in 0..7 {
            let deadline = Instant::now() + PATIENCE;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "Model request {phase} was not made"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = 0;
            let mut authorization = false;
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                if lower.trim() == "authorization: bearer fixture-secret" {
                    authorization = true;
                }
            }
            assert!(authorization, "API key environment reference was not used");
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            if phase == 2 {
                let user = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
                    .unwrap();
                assert_eq!(
                    user["content"],
                    json!([
                        {"type":"text","text":"continue"},
                        {"type":"image_url","image_url":{"url":format!("data:image/png;base64,{model_image}")}}
                    ])
                );
            }
            let mut names: Vec<_> = request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["function"]["name"].as_str().unwrap())
                .collect();
            names.sort_unstable();
            assert_eq!(
                names,
                [
                    "agent", "ask_user", "branch", "edit", "exec", "glob", "grep", "plugin",
                    "read", "skill", "wait", "write"
                ]
            );
            let frame = |delta, finish| {
                format!(
                    "data: {}\n\n",
                    json!({"id":"fixture-response","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                )
            };
            let data = if phase == 0 {
                frame(
                    json!({"tool_calls":[{"index":0,"id":"read-call","type":"function","function":{"name":"read","arguments":"{\"path\":\"marker.txt\"}"}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else if phase == 3 {
                let user = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
                    .unwrap();
                let text = match &user["content"] {
                    Value::String(text) => text.as_str(),
                    Value::Array(parts) => {
                        assert_eq!(parts.len(), 1);
                        assert_eq!(parts[0]["type"], "text");
                        parts[0]["text"].as_str().unwrap()
                    }
                    other => panic!("Unexpected user content: {other}"),
                };
                let path = text
                    .strip_prefix("[Pasted text: ")
                    .unwrap()
                    .split_once(" (")
                    .unwrap()
                    .0;
                assert_eq!(fs::read(path).unwrap(), model_paste.as_bytes());
                frame(
                    json!({"tool_calls":[{"index":0,"id":"paste-read","type":"function","function":{"name":"read","arguments":serde_json::to_string(&json!({"path":path})).unwrap()}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else if phase == 5 {
                let user = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
                    .unwrap();
                let parts = user["content"].as_array().unwrap();
                assert_eq!(parts[0], json!({"type":"text","text":"selected "}));
                assert_eq!(
                    parts[1],
                    json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{model_image}")}})
                );
                assert!(
                    user["content"]
                        .to_string()
                        .contains(model_reference.to_str().unwrap())
                );
                frame(
                    json!({"tool_calls":[{"index":0,"id":"selected-read","type":"function","function":{"name":"read","arguments":serde_json::to_string(&json!({"path":model_reference})).unwrap()}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else {
                if phase == 1 {
                    assert!(
                        request["messages"].as_array().unwrap().iter().any(
                            |message| message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("production-read-marker"))
                        ),
                        "The real read result did not reach the model"
                    );
                } else if phase == 2 {
                    let messages = request["messages"].to_string();
                    assert!(messages.contains("dim-shell-input"));
                    assert!(messages.contains("cli-recorded-marker"));
                    assert!(!messages.contains("cli-private-marker"));
                } else if phase == 4 {
                    assert!(
                        request["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|message| {
                                message["role"] == "tool"
                                    && message["content"].as_str().is_some_and(|text| {
                                        text.contains("native-paste-reader-marker")
                                    })
                            }),
                        "The stored paste did not reach the model through read"
                    );
                } else {
                    assert!(
                        request["messages"].as_array().unwrap().iter().any(
                            |message| message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("outside-attachment-marker"))
                        )
                    );
                }
                frame(
                    json!({"content":match phase {1 => "read completed", 2 => "session continued", 4 => "paste read completed", _ => "files read completed"}}),
                    json!("stop"),
                )
            } + "data: [DONE]\n\n";
            write!(stream.get_mut(),"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    let config = home.0.join("model.json");
    fs::write(&config,serde_json::to_vec(&json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","apiKeyEnv":"DIM_CLI_TEST_API_KEY","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","config":{"maxTokens":128}}]}]})).unwrap()).unwrap();
    let mut first = home.command();
    first
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
            "exec",
            "--prompt",
            "read marker.txt",
        ])
        .env("DIM_CLI_TEST_API_KEY", "fixture-secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = wait(first.spawn().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "read completed\n"
    );
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("[done] read"));
    assert!(!diagnostic.contains("fixture-secret"));
    let session = diagnostic
        .lines()
        .find_map(|line| line.strip_prefix("session: "))
        .unwrap();
    let mut interactive = home.command();
    interactive.args([
        "--server",
        &binary,
        "--model-config",
        config.to_str().unwrap(),
        "--cwd",
        home.0.to_str().unwrap(),
        "--resume",
        session,
    ]);
    let mut terminal = Terminal::launch(interactive);
    terminal.until("\n> ");
    terminal.until("Ctrl+J: newline");
    terminal.write(b"!printf cli-recorded-marker\n");
    terminal.until("[shell recorded] sequence:");
    terminal.until("\n> ");
    terminal.write(b"!!printf cli-private-marker\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    let mut resumed = home.command();
    resumed
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
            "exec",
            "resume",
            session,
            "--prompt",
            "continue",
            "--image",
            image.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = wait(resumed.spawn().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "session continued\n"
    );
    let mut interactive = home.command();
    interactive.args([
        "--server",
        &binary,
        "--model-config",
        config.to_str().unwrap(),
        "--cwd",
        home.0.to_str().unwrap(),
        "--resume",
        session,
    ]);
    let mut terminal = Terminal::launch(interactive);
    terminal.until("\n> ");
    terminal.until("Ctrl+J: newline");
    terminal.paste(&paste);
    terminal.until("[Paste 1: 2001 lines]");
    terminal.write(b"\n");
    terminal.until("paste read completed");
    terminal.editor();
    terminal.write(b"selected ");
    terminal.paste(image.to_str().unwrap());
    terminal.until("[Image 1: screenshot.dat]");
    terminal.paste(url::Url::from_file_path(&reference).unwrap().as_str());
    terminal.until("[File: outside reference.txt]");
    terminal.write(b"\n");
    terminal.until("files read completed");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    let exported = home
        .command()
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "session",
            "export",
            session,
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let conversation: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(conversation["sessionId"], session);
    assert_eq!(conversation["branchId"], "main");
    assert!(conversation["events"].as_array().unwrap().iter().any(|event| {
        event["type"] == "input" && event["name"] == "message"
            && event["data"]["user"]["content"] == json!([
                {"type":"text","text":"continue"},
                {"type":"image","source":{"type":"base64","mediaType":"image/png","data":image_data}}
            ])
    }));
    assert!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["type"] == "input"
                    && event["name"] == "message"
                    && event["data"]["user"]["content"][0]["text"]
                        .as_str()
                        .is_some_and(|text| {
                            text.starts_with("[Pasted text: ") && text.ends_with(" (2001 lines)]")
                        })
            })
    );
    assert_eq!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["blocks"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("<dim-shell-input ")))
            .count(),
        1
    );
    assert!(!conversation.to_string().contains("cli-private-marker"));
    assert!(conversation["events"].as_array().unwrap().iter().any(|event| event["type"] == "input" && event["name"] == "message" && event["data"]["user"]["content"] == json!([
        {"type":"text","text":"selected "},
        {"type":"image","source":{"type":"base64","mediaType":"image/png","data":image_data}},
        {"type":"text","text":reference.to_str().unwrap()}
    ])));
    assert!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["name"] == "text" && event["data"]["delta"] == "session continued")
    );
    assert!(
        !String::from_utf8(exported.stdout)
            .unwrap()
            .contains("fixture-secret")
    );
    model.join().unwrap();
}

fn mcp_catalog(id: &str) -> Value {
    json!({"userRevision":"user:1","projectRevision":"project:1",
        "servers":[{"id":id,"source":"project","transport":"http","enabled":true,
            "auth":{"status":"required","missingScopes":["files:read"]}}],
        "userApplication":{"error":null,"cleanupError":null},"projectApplication":null})
}

fn mcp_hello(wire: &mut Wire) {
    let hello = wire.read("hello");
    wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
}

fn mcp_started(wire: &mut Wire, redirect: &str, expired: bool) {
    let request = wire.read("mcp.auth.start");
    let expires = if expired {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 1200
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 60_000
    };
    wire.reply(&request,json!({"attemptId":"fixture-attempt","authorizationUrl":"https://issuer.example/authorize?state=fixture","redirectUri":redirect,"issuer":"https://issuer.example","resource":"https://resource.example/mcp","expiresAt":expires}));
}

#[test]
fn mcp_sign_in_rejects_noninteractive_and_argument_secrets_before_connecting() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for args in [
        vec!["mcp", "login", "docs"],
        vec!["mcp", "login", "docs", "--client-secret=must-not-print"],
        vec!["mcp", "login", "docs", "--client-secret", "must-not-print"],
    ] {
        let output = home
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-print"));
        assert!(!home.0.join("rust").exists());
    }
}

#[test]
fn mcp_catalog_and_sign_out_use_public_auth_data_and_the_selected_source() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        mcp_hello(wire);
        let request = wire.read("mcp.catalog");
        let mut catalog = mcp_catalog("docs");
        catalog["servers"].as_array_mut().unwrap().extend([
            json!({"id":"authorized","source":"user","transport":"sse","enabled":true,"auth":{"status":"authorized"}}),
            json!({"id":"expired","source":"user","transport":"http","enabled":true,"auth":{"status":"expired"}}),
        ]);
        wire.reply(&request,catalog);
    });
    let output = home.command().args(["mcp", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("sign-in required"));
    assert!(text.contains("signed in"));
    assert!(text.contains("sign-in expired"));
    assert!(text.contains("files:read"));
    fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    let daemon = home.daemon(|wire| {
        mcp_hello(wire);
        let request = wire.read("mcp.catalog");
        wire.reply(&request, mcp_catalog("docs"));
        let logout = wire.read("mcp.auth.logout");
        assert_eq!(logout["params"]["source"], "project");
        assert_eq!(logout["params"]["id"], "docs");
        assert!(logout["params"]["cwd"].is_string());
        wire.reply(&logout, json!({"revoked":false}));
    });
    let output = home
        .command()
        .args(["mcp", "logout", "docs"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Revoked: false"));
}

#[test]
fn interactive_mcp_sign_in_hides_secrets_retries_callbacks_and_drains_before_next_command() {
    let home = Home::new();
    let exported = home.0.join("signed-in.json");
    let (release, finish) = std::sync::mpsc::channel();
    let (accepted, admitted) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,mcp_catalog("docs team"));
        let state = wire.read("session.mcp");
        wire.reply(&state,json!({"sessionId":"session","servers":[{"id":"docs team","state":"auth_required"}],"catalogError":null,"closed":false}));
        for other in ["connecting", "connected"] { wire.notice("session.mcp.state", json!({"sessionId":"session","servers":[{"id":"docs team","state":"auth_required"},{"id":"other","state":other}]})); }
        let invalid = wire.read("mcp.catalog");
        wire.reply(&invalid,mcp_catalog("docs team"));
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,mcp_catalog("docs team"));
        let request = wire.read("mcp.auth.start");
        assert_eq!(request["params"]["source"],"project");
        assert_eq!(request["params"]["clientSecret"],"fixture-confidential-secret");
        let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 60_000;
        wire.reply(&request,json!({"attemptId":"fixture-attempt","authorizationUrl":"https://issuer.example/authorize","redirectUri":"http://localhost:1456/callback","resource":"https://resource.example/mcp","issuer":"https://issuer.example","expiresAt":expires}));
        let callback = wire.read("mcp.auth.complete");
        assert_eq!(callback["params"]["callbackUrl"],"http://localhost:1456/callback?code=wrong-code&state=wrong-state");
        wire.send(json!({"jsonrpc":"2.0","id":callback["id"],"error":{"code":-32000,"message":"invalid callback","data":{"appError":"mcp_auth_callback_invalid"}}}));
        let callback = wire.read("mcp.auth.complete");
        assert_eq!(callback["params"]["attemptId"],"fixture-attempt");
        assert_eq!(callback["params"]["callbackUrl"],"http://localhost:1456/callback?code=private-code&state=right-state");
        accepted.send(()).unwrap();
        finish.recv_timeout(PATIENCE).unwrap();
        wire.reply(&callback,json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let catalog = wire.read("mcp.catalog");
        wire.reply(&catalog,mcp_catalog("docs team"));
        let logout = wire.read("mcp.auth.logout");
        wire.reply(&logout,json!({"revoked":true}));
        let submit=wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle=wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"/mcp\n");
    terminal.until("auth_required");
    terminal.until("\n> ");
    terminal.write(b"/mcp login docs team --no-browser\n");
    terminal.until("Usage: /mcp login");
    terminal.until("\n> ");
    terminal.write(b"/mcp login --client-secret argument-secret docs\n");
    terminal.until("No matching MCP server");
    terminal.until("\n> ");
    terminal.write(b"/mcp login --no-browser --client-secret docs team\n");
    terminal.until("Client secret (hidden): ");
    terminal.write(b"fixture-confidential-secret\n");
    terminal.until("Resource: https://resource.example/mcp");
    terminal.until("Authorization server: issuer.example");
    terminal.until("Callback URL: ");
    terminal.write(b"http://localhost:1456/callback?code=wrong-code&state=wrong-state\n");
    terminal.until("try again");
    terminal.until("Callback URL: ");
    terminal.write(b"http://localhost:1456/callback?code=private-code&state=right-state\n");
    admitted.recv_timeout(PATIENCE).unwrap();
    terminal.signal(libc::SIGINT);
    terminal.until("Waiting for the MCP operation to settle");
    release.send(()).unwrap();
    terminal.until("Signed in to");
    terminal.until("\n> ");
    terminal.write(format!("/export json {}\n", exported.display()).as_bytes());
    terminal.until("Exported JSON:");
    terminal.until("\n> ");
    terminal.write(b"/mcp logout docs team\n");
    terminal.until("Revoked: true");
    terminal.until("\n> ");
    terminal.write(b"continue after sign-out\n");
    terminal.until("sign-in required. Run mcp login");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let data: Value = serde_json::from_slice(&fs::read(exported).unwrap()).unwrap();
    assert_eq!(data["through"], 8);
    assert_eq!(data["events"], json!([]));
    assert!(!terminal.transcript.contains("fixture-confidential-secret"));
    assert_eq!(
        terminal.transcript.matches("argument-secret").count(),
        1,
        "only the terminal echo of the typed command may contain it"
    );
    assert_eq!(
        terminal
            .transcript
            .matches("sign-in required. Run mcp login")
            .count(),
        1
    );
    assert!(!terminal.transcript.contains("wrong-code"));
    assert!(!terminal.transcript.contains("private-code"));
}

/// A loopback port free on both families, for the CLI to bind. It is taken
/// below the ephemeral range: an ephemeral port released for the CLI can be
/// taken meanwhile by an outgoing connection elsewhere on the machine.
fn loopback_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let base = (std::process::id() % 8_000) as u16;
    (0..8_000)
        .map(|_| 20_000 + (base + NEXT.fetch_add(1, Ordering::Relaxed)) % 8_000)
        .find(|&port| {
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
                && std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)).is_ok()
        })
        .expect("a free loopback port")
}

#[test]
fn mcp_loopback_capture_supports_both_families_and_busy_ports_fall_back_to_hidden_input() {
    for mode in ["ipv4", "ipv6", "busy", "paste", "noise", "cancel", "expiry"] {
        let home = Home::new();
        let _reserve = (mode == "busy")
            .then(|| std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap());
        let port = _reserve.as_ref().map_or_else(loopback_port, |listener| {
            listener.local_addr().unwrap().port()
        });
        let redirect = format!("http://localhost:{port}/callback");
        let expected = format!("{redirect}?code=private-callback&state=fixture");
        let daemon = home.daemon(move |wire| {
            mcp_hello(wire);
            let catalog = wire.read("mcp.catalog");
            wire.reply(&catalog, mcp_catalog("docs"));
            mcp_started(wire, &redirect, mode == "expiry");
            if matches!(mode, "cancel" | "expiry") {
                let mut line = String::new();
                assert_eq!(wire.0.read_line(&mut line).unwrap(), 0);
                return;
            }
            let callback = wire.read("mcp.auth.complete");
            assert_eq!(callback["params"]["callbackUrl"], expected);
            wire.reply(&callback, json!({}));
        });
        let bin = home.0.join("bin");
        fs::create_dir(&bin).unwrap();
        for name in ["open", "xdg-open"] {
            let path = bin.join(name);
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut command = home.command();
        command
            .args(["mcp", "login", "docs"])
            .env("PATH", &bin)
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_TTY");
        let mut terminal = Terminal::launch(command);
        if matches!(mode, "cancel" | "expiry") {
            terminal.until("Callback URL: ");
            if mode == "cancel" {
                terminal.signal(libc::SIGINT);
            }
            terminal.until(if mode == "expiry" {
                "Sign-in expired"
            } else {
                "Stopped waiting"
            });
            assert_eq!(
                terminal.wait().code(),
                Some(if mode == "expiry" { 1 } else { 130 })
            );
            assert!(std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
            assert!(std::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, port)).is_err());
            daemon.join().unwrap();
            continue;
        }
        if matches!(mode, "busy" | "paste") {
            if mode == "paste" {
                terminal.until("Waiting for the browser callback");
            }
            terminal.until("Callback URL: ");
            terminal.write(
                format!("http://localhost:{port}/callback?code=private-callback&state=fixture\n")
                    .as_bytes(),
            );
        } else {
            terminal.until("Callback URL: ");
            if mode == "noise" {
                let mut wrong =
                    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
                wrong.set_read_timeout(Some(PATIENCE)).unwrap();
                wrong
                    .write_all(b"GET /unrelated HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    .unwrap();
                let mut response = String::new();
                wrong.read_to_string(&mut response).unwrap();
                assert!(response.contains("404 Not Found"));
                let mut oversized =
                    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
                oversized
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                oversized.write_all(&vec![b'x'; 16385]).unwrap();
                let mut response = Vec::new();
                let _ = oversized.read_to_end(&mut response);
                let mut idle =
                    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
                idle.set_read_timeout(Some(PATIENCE)).unwrap();
                assert_eq!(idle.read(&mut [0]).unwrap(), 0);
            }
            let mut browser = if mode == "ipv6" {
                std::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, port)).unwrap()
            } else {
                std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap()
            };
            browser.set_read_timeout(Some(PATIENCE)).unwrap();
            write!(browser,"GET /callback?code=private-callback&state=fixture HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").unwrap();
            let mut page = String::new();
            browser.read_to_string(&mut page).unwrap();
            assert!(page.contains("Sign-in completed"));
            assert!(!page.contains("private-callback"));
        }
        terminal.until("Signed in to");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(!terminal.transcript.contains("private-callback"));
    }
}

#[test]
fn mcp_sign_in_stops_on_interrupt_or_expiry_without_a_cleanup_request() {
    for expired in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            mcp_hello(wire);
            let catalog = wire.read("mcp.catalog");
            wire.reply(&catalog, mcp_catalog("docs"));
            mcp_started(wire, "http://localhost:1456/callback", expired);
            let mut line = String::new();
            assert_eq!(
                wire.0.read_line(&mut line).unwrap(),
                0,
                "no cancellation RPC exists for MCP attempts"
            );
        });
        let mut command = home.command();
        command.args(["mcp", "login", "--no-browser", "docs"]);
        let mut terminal = Terminal::launch(command);
        if !expired {
            terminal.until("Callback URL: ");
            terminal.signal(libc::SIGINT);
        }
        terminal.until(if expired {
            "Sign-in expired"
        } else {
            "Stopped waiting"
        });
        assert_eq!(terminal.wait().code(), Some(if expired { 1 } else { 130 }));
        daemon.join().unwrap();
    }
}

#[test]
fn mcp_submitted_success_stays_successful_after_interrupt_or_expiry() {
    for expires in [false, true] {
        let home = Home::new();
        let (admitted, accepted) = std::sync::mpsc::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let daemon=home.daemon(move |wire| {
            mcp_hello(wire);
            let catalog=wire.read("mcp.catalog"); wire.reply(&catalog,mcp_catalog("docs"));
            let start=wire.read("mcp.auth.start");
            let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            wire.reply(&start,json!({"attemptId":"held","authorizationUrl":"https://issuer.example/authorize","redirectUri":"http://localhost:1456/callback","resource":"https://resource.example/mcp","issuer":"https://issuer.example","expiresAt":now+if expires {1200} else {60_000}}));
            let callback=wire.read("mcp.auth.complete"); admitted.send(()).unwrap();
            wait_release.recv_timeout(PATIENCE).unwrap();
            wire.reply(&callback,json!({}));
        });
        let mut command = home.command();
        command.args(["mcp", "login", "--no-browser", "docs"]);
        let mut terminal = Terminal::launch(command);
        terminal.until("Callback URL: ");
        terminal.write(b"http://localhost:1456/callback?code=held-secret&state=held\n");
        accepted.recv_timeout(PATIENCE).unwrap();
        if expires {
            terminal.until("Sign-in expired; waiting for the submitted operation");
        } else {
            terminal.signal(libc::SIGINT);
            terminal.until("Waiting for the MCP operation to settle");
        }
        release.send(()).unwrap();
        terminal.until("Signed in to");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(!terminal.transcript.contains("held-secret"));
    }
}

#[test]
fn daemon_start_waits_for_private_permissions_without_trusting_existing_public_sockets() {
    let home = Home::new();
    let server = home.0.join("server");
    let started = home.0.join("server.started");
    fs::write(
        &server,
        "#!/bin/sh\n: > \"$0.started\"\nwhile test -f \"$0\"; do sleep 0.01; done\n",
    )
    .unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = home.command();
    command
        .arg("--server")
        .arg(&server)
        .args(["models", "fixture"]);

    let socket = home.0.join("rust/daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
    let rejected = command.output().unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("private socket"));
    assert!(
        !started.exists(),
        "an existing insecure endpoint must not start a server"
    );
    drop(listener);
    fs::remove_file(&socket).unwrap();

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    while !started.exists() {
        assert!(child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "server was not started");
        thread::sleep(Duration::from_millis(10));
    }
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(
        child.try_wait().unwrap().is_none(),
        "startup must wait for a private endpoint"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let (stream, _) = loop {
        match listener.accept() {
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            result => break result.unwrap(),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut wire = Wire(BufReader::new(stream));
    let hello = wire.read("hello");
    wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
    let catalog = wire.read("provider.catalog");
    wire.reply(
        &catalog,
        json!({"catalog":{"models":[],"disabledModelIds":[]}}),
    );
    let output = wait(child);
    fs::remove_file(&server).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn initial_attach_eof_settles_a_shell_started_before_the_first_prompt() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach, json!({}));
        let history = wire.read("session.history"); wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"restored","textOffset":0,"text":"initial model pending","thinking":"","thinkingOffset":0}));
        let run = wire.read("shell.run");
        wire.reply(&idle, json!({"sequence":8}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"initial shell pending"}}));
        let stop = wire.read("shell.stop"); assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false}));
        wire.reply(&stop, json!({"stopped":true}));
        wire.detach();
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("initial model pending");
    terminal.write(b"!!wait\n");
    terminal.until("initial shell pending");
    terminal.write(&[4]);
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn interrupted_shell_reply_retains_its_run_for_explicit_stop_and_settlement_feedback() {
    let home = Home::new();
    let (release, finish) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        for malformed in [false, true] {
            let run = wire.read("shell.run");
            let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":8}));
            if malformed { wire.reply(&run, json!({"runId":"wrong","status":"completed","output":"","truncated":false})); }
            else { wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32000,"message":"Reloaded","data":{"appError":"operation_interrupted"}}})); }
            let stop = wire.read("shell.stop"); assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
            if !malformed { finish.recv_timeout(PATIENCE).unwrap(); }
            if !malformed {
                wire.send(json!({"jsonrpc":"2.0","id":stop["id"],"error":{"code":-32000,"message":"Reloaded","data":{"appError":"operation_interrupted"}}}));
                let retry=wire.read("shell.stop"); assert_ne!(retry["id"], stop["id"]); assert_eq!(retry["params"]["runId"], run["params"]["runId"]);
                wire.reply(&retry,json!({"stopped":false}));
            } else { wire.reply(&stop, json!({"stopped":false})); }
        }
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    for malformed in [false, true] {
        terminal.write(b"!held\n");
        terminal.until("[shell interrupted] Run");
        terminal.signal(libc::SIGINT);
        terminal.until("[shell stopping] Waiting for command settlement");
        if !malformed {
            terminal.signal(libc::SIGINT);
            terminal.until("Still waiting for command settlement");
            release.send(()).unwrap();
            terminal.until("Stop was not confirmed");
            terminal.signal(libc::SIGINT);
            terminal.until("[shell stopping] Waiting for command settlement");
        }
        terminal.until("\n> ");
    }
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn approval_interrupt_stops_the_turn_instead_of_a_deferred_completed_shell() {
    let home = Home::new();
    let (release, finish) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"working","textOffset":0,"text":"model running","thinking":"","thinkingOffset":0}));
        let run = wire.read("shell.run");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"command":"inspect"}},"reason":"review"}));
        wire.reply(&run,json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":"deferred output","truncated":false}));
        let stop=wire.read("session.stop");
        finish.recv_timeout(PATIENCE).unwrap();
        wire.notice("approval.cancelled",json!({"sessionId":"session","requestId":"approval"}));
        wire.reply(&idle,json!({"sequence":40})); wire.reply(&stop,json!({"sequence":40}));
        let next = wire.read("session.submit");
        assert_eq!(next["params"]["text"], "after interrupts");
        wire.reply(&next,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle,json!({"sequence":40}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"work\n");
    terminal.until("model running");
    terminal.write(b"!inspect\n");
    terminal.until("Allow once? [y/N] ");
    assert!(!terminal.transcript.contains("deferred output"));
    terminal.signal(libc::SIGINT);
    terminal.until("[stopping] Waiting for resource settlement");
    terminal.signal(libc::SIGINT);
    terminal.until("[stopping] Still waiting for resource settlement");
    release.send(()).unwrap();
    terminal.until("\n> ");
    terminal.write(b"after interrupts\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("[shell stopping]"));
    assert!(
        terminal.transcript.find("deferred output").unwrap()
            > terminal.transcript.find("[stopping]").unwrap()
    );
}

#[test]
fn web_prints_the_entry_and_holds_it_until_interrupted() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","web.open"]}));
        let open = wire.read("web.open");
        assert_eq!(open["params"], json!({"port": 4567}));
        wire.reply(&open, json!({"url":"http://127.0.0.1:4567/#token=fixture-token","address":"127.0.0.1:4567"}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command
        .args(["web", "--port", "4567", "--no-open"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut panel = String::new();
    for _ in 0..3 {
        stdout.read_line(&mut panel).unwrap();
    }
    assert_eq!(
        panel,
        "Web workspace  http://127.0.0.1:4567/\nOpen this link to sign in: http://127.0.0.1:4567/#token=fixture-token\nPress Ctrl+C to stop the Web workspace.\n"
    );
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "\nWeb workspace stopped.\n");
    let output = wait(child);
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn web_off_clears_the_daemon_switch() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","web.disable"]}));
        let disable = wire.read("web.disable");
        assert_eq!(disable["params"], json!({}));
        wire.reply(&disable, json!({"enabled":false,"open":false,"connections":0}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home.command().args(["web", "--off"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Web switch off.\n");
    daemon.join().unwrap();
}

#[test]
fn web_without_the_web_workspace_says_how_to_get_it() {
    for installed in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","web.open"]}));
            let open = wire.read("web.open");
            wire.send(json!({"jsonrpc":"2.0","id":open["id"],"error":{"code":-32000,"message":"This App Server has no Web workspace","data":{"appError":"web_assets_missing"}}}));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut command = home.command();
        command
            .args(["web", "--no-open"])
            .env_remove("DIMCODE_WEB_ASSETS");
        if installed {
            let assets = home.0.join("npm-web");
            fs::create_dir_all(&assets).unwrap();
            fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
            command.env("DIMCODE_WEB_ASSETS", assets);
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(output.stdout.is_empty());
        let expected = if installed {
            "The Web workspace was installed after the daemon started.\nRestart the daemon with `dimcode daemon stop`, then try again.\n"
        } else {
            "The Web workspace is not installed.\nInstall it with `dimcode web install`.\n"
        };
        assert_eq!(stderr, expected);
    }
}

/// A registry that serves `dimcode-web@<this version>` with `integrity`
/// until the test ends; its base URL.
fn web_registry(tarball: Vec<u8>, integrity: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let manifest =
        json!({"dist":{"tarball":format!("{base}/dimcode-web.tgz"),"integrity":integrity}})
            .to_string();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut wire = BufReader::new(stream.unwrap());
            let mut request = String::new();
            wire.read_line(&mut request).unwrap();
            let mut line = String::new();
            while wire.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let body = if request
                .starts_with(&format!("GET /dimcode-web/{} ", env!("CARGO_PKG_VERSION")))
            {
                manifest.as_bytes().to_vec()
            } else {
                assert!(request.starts_with("GET /dimcode-web.tgz "), "{request}");
                tarball.clone()
            };
            let stream = wire.get_mut();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    base
}

#[test]
fn web_install_puts_this_versions_frontend_in_the_home() {
    use base64::Engine;
    use sha2::Digest;
    let home = Home::new();
    let package = home.0.join("pack/package");
    fs::create_dir_all(package.join("web/assets")).unwrap();
    fs::write(package.join("package.json"), "{}").unwrap();
    fs::write(package.join("web/index.html"), "<!doctype html>").unwrap();
    fs::write(package.join("web/assets/app-1.js"), "export {}").unwrap();
    let archive = home.0.join("pack/dimcode-web.tgz");
    assert!(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(home.0.join("pack"))
            .arg("package")
            .status()
            .unwrap()
            .success()
    );
    let tarball = fs::read(&archive).unwrap();
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&tarball))
    );
    fs::create_dir_all(home.0.join("web")).unwrap();
    fs::write(home.0.join("web/stale.js"), "").unwrap();

    let output = home
        .command()
        .args(["web", "install"])
        .env(
            "DIMCODE_NPM_REGISTRY",
            web_registry(tarball.clone(), integrity.clone()),
        )
        .env_remove("DIMCODE_WEB_ASSETS")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        stdout,
        format!(
            "Downloading dimcode-web@{version}…\nWeb workspace {version} installed in {}.\n",
            home.0.join("web").display()
        )
    );
    assert_eq!(
        fs::read_to_string(home.0.join("web/assets/app-1.js")).unwrap(),
        "export {}"
    );
    assert!(home.0.join("web/index.html").is_file());
    assert!(!home.0.join("web/stale.js").exists());
    assert!(!home.0.join("web/package.json").exists());
    assert!(!home.0.join("web.partial").exists());

    let output = home
        .command()
        .args(["web", "install"])
        .env(
            "DIMCODE_NPM_REGISTRY",
            web_registry(tarball.clone(), format!("sha512-{}", "A".repeat(88))),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&format!(
            "dimcode-web@{version} does not match its registry integrity"
        )),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(home.0.join("web/assets/app-1.js").is_file());

    let first = home.0.join("npm-web");
    fs::create_dir_all(&first).unwrap();
    fs::write(first.join("index.html"), "<!doctype html>").unwrap();
    let output = home
        .command()
        .args(["web", "install"])
        .env("DIMCODE_NPM_REGISTRY", web_registry(tarball, integrity))
        .env("DIMCODE_WEB_ASSETS", &first)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).ends_with(&format!(
            "The Web workspace in {} is used instead; remove it to use this one.\n",
            first.display()
        )),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn tui_web_toggles_the_daemon_switch_without_leaving() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","web.toggle"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let toggle = wire.read("web.toggle");
        assert_eq!(toggle["params"], json!({}));
        wire.reply(
            &toggle,
            json!({"enabled":true,"open":true,"url":"http://127.0.0.1:4567/#token=fixture-token"}),
        );
        let toggle = wire.read("web.toggle");
        wire.reply(&toggle, json!({"enabled":false,"open":false}));
        let toggle = wire.read("web.toggle");
        wire.send(json!({"jsonrpc":"2.0","id":toggle["id"],"error":{"code":-32000,"message":"This App Server has no Web workspace","data":{"appError":"web_assets_missing"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command
        .env("DIMCODE_OAUTH_MANUAL", "1")
        .env_remove("DIMCODE_WEB_ASSETS");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/web\r");
    terminal.until("Web interface on: http://127.0.0.1:4567/chat/session#token=fixture-token");
    terminal.write(b"/web\r");
    terminal.until("Web interface off");
    terminal.write(b"/web\r");
    terminal.until("The Web workspace is not installed. Install it with `dimcode web install`.");
    assert!(!terminal.transcript.contains("RPC"));
    terminal.write(b"/exit\r");
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success());
}

#[test]
fn tui_stays_up_when_the_daemon_leaves_and_restores_the_terminal_on_exit() {
    let home = Home::new();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wait_release.recv_timeout(PATIENCE).unwrap();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"keep this draft");
    terminal.until("keep this draft");
    release.send(()).unwrap();
    daemon.join().unwrap();
    terminal.until("Error: App Server connection closed");
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe {
            libc::tcgetattr(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                attributes.as_mut_ptr(),
            )
        },
        0
    );
    assert_eq!(unsafe { attributes.assume_init() }.c_lflag & libc::ECHO, 0);
    terminal.until("dimcode --resume session");
    terminal.write(b"\r");
    terminal.until("Disconnected; draft kept");
    assert!(!home.0.join("input-history.json").exists());
    terminal.write(b"\x04");
    terminal.until("Bye!");
    terminal.until("\x1b[?2004l");
    let status = terminal.wait();
    assert!(status.success());
    assert_eq!(
        unsafe {
            libc::tcgetattr(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                attributes.as_mut_ptr(),
            )
        },
        0
    );
    assert_ne!(unsafe { attributes.assume_init() }.c_lflag & libc::ECHO, 0);
}

/// Provider edits use the source-built daemon, actual HTTP and a disposable home.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn tui_provider_management_uses_the_production_daemon_and_survives_restart() {
    fn rpc(wire: &mut BufReader<UnixStream>, method: &str, params: Value) -> Value {
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
        )
        .unwrap();
        wire.get_mut().flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                wire.read_line(&mut line).unwrap() > 0,
                "{method}: connection closed"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == 1 {
                return value;
            }
        }
    }
    fn connect(home: &Home) -> BufReader<UnixStream> {
        let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let mut wire = BufReader::new(stream);
        let hello = rpc(
            &mut wire,
            "hello",
            json!({"protocolVersion":3,"clientKind":"cli"}),
        );
        assert!(hello.get("result").is_some(), "{hello}");
        wire
    }
    fn post(events: &std::sync::mpsc::Receiver<(String, Value)>, marker: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let (key, body) = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if body.to_string().contains(marker) {
                assert!(matches!(
                    key.as_str(),
                    "Bearer first-key" | "Bearer second-key"
                ));
                assert_eq!(body["reasoning_effort"], "high");
                return json!({"key":key,"body":body});
            }
        }
    }
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (send, events) = std::sync::mpsc::channel();
    let http = thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = stream.unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(8)))
                .unwrap();
            let mut wire = BufReader::new(stream);
            let mut line = String::new();
            wire.read_line(&mut line).unwrap();
            let request_line = line.clone();
            let mut length = 0;
            let mut key = String::new();
            loop {
                line.clear();
                wire.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                if name.eq_ignore_ascii_case("authorization") {
                    key = value.trim().to_owned();
                }
                if name.eq_ignore_ascii_case("x-api-key") {
                    key = format!("x-api-key {}", value.trim());
                }
            }
            let mut bytes = vec![0; length];
            wire.read_exact(&mut bytes).unwrap();
            let stop = request_line.starts_with("GET /stop ");
            let (content_type, body) = if stop {
                ("text/plain", "done".to_owned())
            } else if request_line.starts_with("GET /v1/models ") {
                assert!(matches!(
                    key.as_str(),
                    "Bearer first-key" | "Bearer second-key"
                ));
                ("application/json", json!({"data":[{"id":"fixture-model","context_window":32000,"max_output_tokens":256}]}).to_string())
            } else if request_line.starts_with("POST /v1/messages ") {
                assert_eq!(key, "x-api-key first-key");
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let model = body["model"].clone();
                send.send((key, body)).unwrap();
                let frames = [
                    json!({"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0}}}),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"builtin-provider-reply"}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}),
                    json!({"type":"message_stop"}),
                ];
                (
                    "text/event-stream",
                    frames
                        .into_iter()
                        .map(|frame| {
                            format!(
                                "event: {}\ndata: {frame}\n\n",
                                frame["type"].as_str().unwrap()
                            )
                        })
                        .collect::<String>(),
                )
            } else {
                assert!(
                    request_line.starts_with("POST /v1/chat/completions "),
                    "{request_line}"
                );
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                send.send((key.clone(), body)).unwrap();
                let text = if key == "Bearer first-key" {
                    "provider-first-reply"
                } else {
                    "provider-updated-reply"
                };
                (
                    "text/event-stream",
                    format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"id":"provider-flow","choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]}),
                        json!({"id":"provider-flow","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
                    ),
                )
            };
            write!(wire.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            wire.get_mut().flush().unwrap();
            if stop {
                break;
            }
        }
    });
    let command = || {
        let mut command = home.command();
        command.args(["--server", &binary, "--cwd", home.0.to_str().unwrap()]);
        command
    };
    let mut terminal = Terminal::launch_tui(command());
    terminal.until("Sign in with /login");
    terminal.write(b"/connect\r");
    terminal.until("disabled");
    terminal.write(b"Add custom provider\r");
    terminal.until("Name:");
    terminal.write(format!("Provider flow\rhttp://{address}/v1\rfirst-key\r\r").as_bytes());
    terminal.until("Connect ");
    terminal.until("Disconnect");
    terminal.until("Added Provider flow");
    let mut wire = connect(&home);
    let snapshot = rpc(&mut wire, "provider.management", json!({}));
    let provider = snapshot["result"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["displayName"] == "Provider flow")
        .unwrap();
    let id = provider["providerId"].as_str().unwrap().to_owned();
    assert_eq!(uuid::Uuid::parse_str(&id).unwrap().to_string(), id);
    assert_eq!(provider["version"], 1);
    assert_eq!(provider["models"][0]["model"]["modelId"], "fixture-model");
    assert_eq!(provider["models"][0]["source"], "default");
    assert!(!snapshot.to_string().contains("first-key"));
    assert!(rpc(&mut wire, "provider.models.edit", json!({"providerId":id,"expectedVersion":1,"modelId":"fixture-model","edit":{"reasoning":{"supported":true,"effortOptions":["low","high"]}}})).get("result").is_some());
    assert!(rpc(&mut wire, "provider.preferences.update", json!({"providerId":id,"expectedVersion":2,"modelId":"fixture-model","patch":{"reasoningEffort":{"action":"set","value":"high"}}})).get("result").is_some());
    assert!(
        rpc(
            &mut wire,
            "provider.select",
            json!({"providerId":id,"modelId":"fixture-model"})
        )
        .get("result")
        .is_some()
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    let selected = || {
        let mut command = command();
        command.args(["--provider", &id, "--model", "fixture-model"]);
        command
    };
    let mut terminal = Terminal::launch_tui(selected());
    terminal.until("Ask anything...");
    terminal.write(b"first-provider-message\r");
    terminal.until("provider-first-reply");
    assert_eq!(
        post(&events, "first-provider-message")["key"],
        "Bearer first-key"
    );
    terminal.write(b"/connect\r");
    terminal.until("disabled");
    terminal.write(b"Provider flow\r");
    terminal.until("Connect ");
    assert!(
        rpc(
            &mut wire,
            "provider.update",
            json!({"providerId":id,"expectedVersion":3,
        "patch":{"network":{"timeoutMs":{"action":"set","value":9000}}}})
        )
        .get("result")
        .is_some()
    );
    terminal.write(b"second-key\r\r");
    terminal.until("Provider changed");
    terminal.write(b"\r");
    terminal.until("Connected Provider flow");
    let updated = rpc(&mut wire, "provider.management", json!({"providerId":id}));
    assert_eq!(updated["result"]["providers"][0]["version"], 5);
    assert_eq!(
        updated["result"]["providers"][0]["preferences"]["fixture-model"]["reasoningEffort"],
        "high"
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    let mut terminal = Terminal::launch_tui(selected());
    terminal.until("Ask anything...");
    terminal.write(b"updated-provider-message\r");
    terminal.until("provider-updated-reply");
    assert_eq!(
        post(&events, "updated-provider-message")["key"],
        "Bearer second-key"
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    assert!(
        rpc(&mut wire, "daemon.shutdown", json!({}))
            .get("result")
            .is_some()
    );
    drop(wire);
    let deadline = Instant::now() + Duration::from_secs(8);
    while home.0.join("rust/daemon.sock").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let mut terminal = Terminal::launch_tui(selected());
    terminal.until("Ask anything...");
    terminal.write(b"restored-provider-message\r");
    terminal.until("provider-updated-reply");
    assert_eq!(
        post(&events, "restored-provider-message")["key"],
        "Bearer second-key"
    );
    let mut wire = connect(&home);
    assert!(
        rpc(
            &mut wire,
            "provider.models.edit",
            json!({"providerId":id,"expectedVersion":5,
        "modelId":"fixture-model","edit":{"reasoning":{"supported":true,"effortOptions":["low"]}}})
        )
        .get("result")
        .is_some()
    );
    terminal.write(b"/models\r");
    terminal.until("Provider flow /");
    terminal.write(b"Provider flow\r");
    terminal.until("Model adoption with saved preferences");
    let retained = rpc(&mut wire, "provider.management", json!({"providerId":id}));
    assert_eq!(
        retained["result"]["providers"][0]["preferences"]["fixture-model"]["reasoningEffort"],
        "high"
    );
    terminal.write(b"/models\r");
    terminal.until("Provider flow /");
    terminal.write(b"Provider flow");
    assert!(
        rpc(
            &mut wire,
            "provider.update",
            json!({"providerId":id,"expectedVersion":6,
        "patch":{"network":{"timeoutMs":{"action":"set","value":10000}}}})
        )
        .get("result")
        .is_some()
    );
    terminal.write(b"\x18");
    terminal.until("Provider version changed");
    assert_eq!(
        rpc(&mut wire, "provider.management", json!({"providerId":id}))["result"]["providers"][0]["preferences"]
            ["fixture-model"]["reasoningEffort"],
        "high"
    );
    terminal.write(b"\x18");
    terminal.until("Saved model preferences cleared");
    assert_eq!(
        rpc(&mut wire, "provider.management", json!({"providerId":id}))["result"]["providers"][0]["preferences"],
        json!({})
    );
    terminal.write(b"\r");
    terminal.until("Ask anything...");
    let output = home
        .command()
        .args(["models", &id, "--clear-preferences", "fixture-model"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Saved model preferences cleared"));
    terminal.write(b"/connect\r");
    terminal.until("disabled");
    terminal.write(b"Provider flow\r");
    terminal.until("Connect ");
    terminal.write(b"\r\r");
    terminal.until("Disconnected Provider flow");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    let disconnected = rpc(&mut wire, "provider.management", json!({"providerId":id}));
    let provider = &disconnected["result"]["providers"][0];
    assert_eq!(provider["enabled"], false);
    assert_eq!(provider["connection"]["credentialConfigured"], false);
    assert_eq!(provider["version"], 10);
    assert!(
        rpc(
            &mut wire,
            "provider.delete",
            json!({"providerId":id,"expectedVersion":10})
        )
        .get("result")
        .is_some()
    );
    assert_eq!(
        rpc(&mut wire, "provider.selection", json!({}))["result"]["selection"],
        Value::Null
    );
    assert_eq!(
        rpc(&mut wire, "provider.management", json!({"providerId":id}))["error"]["data"]["appError"],
        "provider_not_found"
    );
    assert!(
        rpc(
            &mut wire,
            "session.create",
            json!({"cwd":home.0,"model":{"provider":id,"modelId":"fixture-model"}})
        )
        .get("error")
        .is_some()
    );
    let anonymous_id = "018ec73f-4d19-7f61-bc79-7130618bb455";
    let created = rpc(
        &mut wire,
        "provider.create",
        json!({"providerId":anonymous_id,"displayName":"Anonymous fixture","connection":{"driverKind":"openai-compatible","auth":{"type":"anonymous"},"baseUrl":format!("http://{address}/v1")},"models":[{"modelId":"anonymous-fixture-model","displayName":"Anonymous fixture model","status":"active","capabilities":dim_agent_types::ModelCapabilities::default()}],"enabled":true}),
    );
    assert!(created.get("result").is_some(), "{created}");
    let mut terminal = Terminal::launch_tui(command());
    terminal.until("Ask anything...");
    terminal.write(b"/models\r");
    terminal.until("Anonymous fixture /");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    assert!(
        rpc(
            &mut wire,
            "provider.delete",
            json!({"providerId":anonymous_id,"expectedVersion":1})
        )
        .get("result")
        .is_some()
    );
    let before = rpc(
        &mut wire,
        "provider.management",
        json!({"providerId":"kimi-for-coding"}),
    );
    let version = before["result"]["providers"][0]["version"]
        .as_u64()
        .unwrap();
    let mut terminal = Terminal::launch_tui(command());
    terminal.until("Sign in with /login");
    terminal.write(b"/connect\r");
    terminal.until("disabled");
    terminal.write(b"Kimi For Coding\r");
    terminal.until("Connect kimi-for-coding");
    terminal.write(b"\x1b[A\x15");
    terminal.write(format!("http://{address}/v1\rfirst-key\r\r").as_bytes());
    terminal.until("Connected Kimi For Coding; saved without testing");
    let builtin = rpc(
        &mut wire,
        "provider.management",
        json!({"providerId":"kimi-for-coding"}),
    );
    let builtin = &builtin["result"]["providers"][0];
    assert_eq!(builtin["version"], version + 1);
    assert_eq!(builtin["enabled"], true);
    assert_eq!(builtin["connection"]["credentialConfigured"], true);
    assert_eq!(
        builtin["connection"]["baseUrl"],
        format!("http://{address}/v1")
    );
    let builtin_model = builtin["models"][0]["model"]["modelId"].as_str().unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    let mut builtin_command = command();
    builtin_command.args(["--provider", "kimi-for-coding", "--model", builtin_model]);
    let mut terminal = Terminal::launch_tui(builtin_command);
    terminal.until("Ask anything...");
    terminal.write(b"builtin-without-listing-message\r");
    terminal.until("builtin-provider-reply");
    loop {
        let (key, body) = events.recv_timeout(Duration::from_secs(8)).unwrap();
        if body.to_string().contains("builtin-without-listing-message") {
            assert_eq!(key, "x-api-key first-key");
            assert_eq!(body["model"], builtin_model);
            break;
        }
    }
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    drop(wire);
    let mut stop = std::net::TcpStream::connect(address).unwrap();
    write!(stop, "GET /stop HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut response = String::new();
    stop.read_to_string(&mut response).unwrap();
    http.join().unwrap();
}

/// The daemon's UI registry (Tui design §3.6): the interface registers
/// after attaching, detaches from a Session it leaves, and leaves with its
/// reason on screen when the daemon asks it to; the Session is not closed.
#[test]
fn tui_registers_with_the_ui_registry_and_leaves_when_asked() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[
            "ui.open",
            "ui.close",
            "ui.attach",
            "ui.status",
            "ui.shutdown",
            "ui.replace",
        ]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let register = wire.read("ui.attach");
        assert_eq!(
            register["params"],
            json!({"sessionId":"session","kind":"tui"})
        );
        wire.reply(&register, json!({"uiId":"ui-1"}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let create = wire.read("session.create");
        wire.reply(&create, json!({"sessionId":"next","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let detach = wire.read("session.detach");
        assert_eq!(detach["params"], json!({"sessionId":"session"}));
        wire.reply(&detach, json!({}));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"next"}));
        wire.reply(&attach, json!({}));
        let register = wire.read("ui.attach");
        assert_eq!(register["params"], json!({"sessionId":"next","kind":"tui"}));
        wire.reply(&register, json!({"uiId":"ui-2"}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.notice("ui.shutdown", json!({"uiId":"ui-1","reason":"not you"}));
        wire.notice(
            "ui.shutdown",
            json!({"uiId":"ui-2","reason":"another terminal took over"}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/new\r");
    terminal.until("The App Server closed this interface: another terminal took over");
    terminal.until("Bye!");
    terminal.until("\x1b[?2004l");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(
        !terminal.transcript.contains("not you"),
        "{}",
        terminal.transcript
    );
}

#[test]
fn tui_queue_shows_held_messages_and_steers_or_withdraws_them() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.submit");
        assert_eq!(first["params"]["text"], "first");
        wire.reply(&first, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"first"}]},"mode":"queue"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-1","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-1","mode":"queue"}}}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":true}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"child","running":true}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"child","running":false}));
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"child","requestId":"child-request","textOffset":0,"text":"child-only answer","thinkingOffset":0,"thinking":"child-only reasoning"}));
        let child_history = wire.read("session.history");
        assert_eq!(child_history["params"]["sessionId"], "session");
        assert_eq!(child_history["params"]["branchId"], "child");
        wire.reply(&child_history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"note-1","sequence":11,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"background task finished"}]},"mode":"append"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-note","sequence":12,"create_at":1,"type":"input","name":"applied","data":{"input_id":"note-1","mode":"append"}}}));
        let second = wire.read("session.submit");
        assert_eq!(second["params"]["text"], "second");
        wire.reply(&second, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-2","sequence":13,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"second"}]},"mode":"queue"}}}));
        let steer = wire.read("session.input.withdraw");
        assert_eq!(steer["params"]["sessionId"], "session");
        assert_eq!(steer["params"]["branchId"], "main");
        assert_eq!(steer["params"]["inputId"], "in-2");
        assert_eq!(steer["params"]["steer"], json!({"content":[{"type":"text","text":"second"}]}));
        wire.reply(&steer, json!({"status":"deleted","sequence":14,"steer":{"inputId":"in-3","sequence":15}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-2","sequence":14,"create_at":1,"type":"input","name":"deleted","data":{"input_id":"in-2"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-3","sequence":15,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"second"}]},"mode":"steer"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-3","sequence":16,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-3","mode":"steer"}}}));
        let third = wire.read("session.submit");
        assert_eq!(third["params"]["text"], "third");
        wire.reply(&third, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-4","sequence":17,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"third"}]},"mode":"queue"}}}));
        let withdraw = wire.read("session.input.withdraw");
        assert_eq!(withdraw["params"]["inputId"], "in-4");
        assert!(withdraw["params"].get("steer").is_none());
        wire.reply(&withdraw, json!({"status":"deleted","sequence":18}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-4","sequence":18,"create_at":1,"type":"input","name":"deleted","data":{"input_id":"in-4"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"first\r");
    terminal.until("first");
    terminal.until("background task finished");
    terminal.write(b"second\r");
    terminal.until("Queue (1)");
    assert!(
        !terminal.transcript.contains("Queue (0)"),
        "{}",
        terminal.transcript
    );
    terminal.until("› second");
    terminal.until("working…");
    assert!(
        !terminal.transcript.contains("┃ second"),
        "{}",
        terminal.transcript
    );
    terminal.write(b"\x13");
    terminal.write(b"third\r");
    terminal.until("Queue (1)");
    terminal.until("› third");
    terminal.write(b"\x1b[A");
    terminal.until("third");
    let plain = {
        let mut out = String::new();
        let mut rest = terminal.transcript.as_str();
        while let Some(start) = rest.find('\u{1b}') {
            out.push_str(&rest[..start]);
            let tail = &rest[start + 1..];
            let skipped = match tail.chars().next() {
                Some('[') => tail
                    .char_indices()
                    .skip(1)
                    .find(|(_, c)| c.is_ascii_alphabetic())
                    .map_or(tail.len(), |(i, c)| i + c.len_utf8()),
                Some(']') => [("\u{7}", 1), ("\u{1b}\\", 2)]
                    .iter()
                    .filter_map(|(end, len)| tail.find(end).map(|i| i + len))
                    .min()
                    .unwrap_or(tail.len()),
                Some(c) => c.len_utf8(),
                None => 0,
            };
            rest = &tail[skipped..];
        }
        out.push_str(rest);
        out
    };
    assert!(!plain.contains("Queue (0)"), "{plain}");
    assert!(plain.contains("┃ first"), "{plain}");
    assert!(plain.contains("┃ second"), "{plain}");
    assert!(!plain.contains("┃ third"), "{plain}");
    assert!(
        !plain.contains("child-only answer")
            && !plain.contains("child-only reasoning")
            && !plain.contains("Thinking…")
            && !plain.contains("thinking…"),
        "{plain}"
    );
    assert!(
        !plain.contains("already left the queue") && !plain.contains("not available"),
        "{plain}"
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_command_output_refreshes_one_read_and_preserves_its_branch_and_terminal_result() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let (proceed, control) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"output-input","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"go"}]},"mode":"queue"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"output-adopted","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"output-input"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"child","event":{"id":"child-start","sequence":1,"create_at":1,"type":"exec","name":"started","data":{"turn_id":"t","request_id":"r","call_id":"child-call","task_id":"child-task","description":"cargo cx build","output_path":"/daemon/private/output"}}}));
        let child_history = wire.read("session.history");
        assert_eq!(child_history["params"]["branchId"], "child");
        wire.reply(&child_history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
        let expected = json!({"sessionId":"session","branchId":"child","taskId":"child-task"});
        let first = wire.read("session.exec.output");
        assert_eq!(first["params"], expected);
        wire.reply(&first, json!({"text":""}));
        let refreshed = wire.read("session.exec.output");
        assert_eq!(refreshed["params"], expected);
        wire.reply(&refreshed, json!({"text":"first \u{1b}]52;c;must-not-copy\u{7}live preview"}));
        let held = wire.read("session.exec.output");
        wire.quiet(Duration::from_millis(1200));
        ready.send(()).unwrap();
        control.recv_timeout(PATIENCE).unwrap();
        wire.reply(&held, json!({"text":"late reply after close"}));
        wire.quiet(Duration::from_millis(1200));
        ready.send(()).unwrap();
        let reopened = wire.read("session.exec.output");
        assert_eq!(reopened["params"], expected);
        wire.reply(&reopened, json!({"text":format!("retained preview\n{}\nretained tail", "long output ".repeat(20))}));
        let unavailable = wire.read("session.exec.output");
        wire.send(json!({"jsonrpc":"2.0","id":unavailable["id"],"error":{"code":-32000,"message":"No live output is available for this task","data":{"appError":"exec_task_unknown"}}}));
        let stop = wire.read("session.exec.stop");
        assert_eq!(stop["params"], expected);
        wire.reply(&stop, json!({"task_id":"child-task","status":"completed","result":{"content":"final child result"}}));
        wire.quiet(Duration::from_millis(1200));
        ready.send(()).unwrap();
        control.recv_timeout(PATIENCE).unwrap();
        wire.notice("session.event", json!({"sessionId":"session","branchId":"child","event":{"id":"child-end","sequence":2,"create_at":1,"type":"exec","name":"ended","data":{"task_id":"child-task","status":"completed","result":{"content":"final child result"}}}}));
        let continued = wire.read("session.submit");
        assert_eq!(continued["params"]["text"], "continue after output");
        wire.reply(&continued, json!({"event":{}}));
        wire.event(11, "text", json!({"turn_id":"t","request_id":"r","delta":"main remains usable"}));
        control.recv_timeout(PATIENCE).unwrap();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"go\r");
    terminal.until("1 in background");
    terminal.write(b"/tasks\r");
    terminal.until("Background tasks");
    terminal.until("cargo cx build");
    terminal.write(b"cx");
    terminal.until("> cx");
    terminal.write(b"\r");
    terminal.until("No output yet");
    terminal.until("first live preview");
    readiness.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\x1b[27u");
    terminal.until("Background tasks");
    proceed.send(()).unwrap();
    readiness.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\r");
    terminal.until("retained preview");
    terminal.until("Live output unavailable");
    let size = libc::winsize {
        ws_row: 18,
        ws_col: 64,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    terminal.until("Live output unavailable");
    terminal.until("retained tail");
    terminal.write(b"x");
    readiness.recv_timeout(PATIENCE).unwrap();
    assert!(
        !terminal.transcript.contains("Completed"),
        "unknown capture or stop reply fabricated an end"
    );
    proceed.send(()).unwrap();
    terminal.until("Completed");
    terminal.until("final child result");
    terminal.write(b"\x1b[27u");
    terminal.until("Background tasks");
    terminal.write(b"\r");
    terminal.until("final child result");
    terminal.write(b"\x1b[27u");
    terminal.until("Background tasks");
    terminal.write(b"\x1b[27u\x1b[27ucontinue after output\r");
    terminal.until("main remains usable");
    assert!(!terminal.transcript.contains("must-not-copy"));
    proceed.send(()).unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_command_output_errors_stop_refresh_and_reopening_retries() {
    for (app_error, expected) in [
        ("unknown_method", "Live output unavailable in this profile"),
        ("service_reloading", "Output read failed"),
    ] {
        let home = Home::new();
        let (ready, readiness) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let submit = wire.read("session.submit");
            wire.reply(&submit, json!({"event":{}}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"error-input","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"go"}]},"mode":"queue"}}}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"running","sequence":10,"create_at":1,"type":"exec","name":"started","data":{"turn_id":"t","request_id":"r","call_id":"call","task_id":"task","description":"command with unavailable output","output_path":"/daemon/private/output"}}}));
            let read = wire.read("session.exec.output");
            wire.reply(&read, json!({"text":"last known preview"}));
            let read = wire.read("session.exec.output");
            wire.send(json!({"jsonrpc":"2.0","id":read["id"],"error":{"code":if app_error == "unknown_method" { -32601 } else { -32000 },"message":"read unavailable","data":{"appError":app_error}}}));
            wire.quiet(Duration::from_millis(1200));
            ready.send(()).unwrap();
            let retried = wire.read("session.exec.output");
            wire.reply(&retried, json!({"text":"fresh preview after reopening"}));
        });
        let mut terminal = Terminal::launch_tui(home.command());
        terminal.until("test / model");
        terminal.write(b"go\r");
        terminal.until("1 in background");
        terminal.write(b"/tasks\r");
        terminal.until("command with unavailable output");
        terminal.write(b"\r");
        terminal.until("last known preview");
        terminal.until(expected);
        readiness.recv_timeout(PATIENCE).unwrap();
        terminal.write(b"\x1b[27u");
        terminal.until("Background tasks");
        terminal.write(b"\r");
        terminal.until("fresh preview after reopening");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_tasks_panel_stops_the_selected_command_or_member() {
    let home = Home::new();
    let (deliver, delivery) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let create = wire.read("session.create");
        wire.reply(&create,json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"go"}]},"mode":"queue"}}}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":true}));
        wire.event(10, "tool-use", json!({"turn_id":"t","request_id":"r","call":{"id":"call-1","name":"exec","input":{"command":"sleep 100","runInBackground":true}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ex-1","sequence":11,"create_at":1,"type":"exec","name":"started","data":{"turn_id":"t","request_id":"r","call_id":"call-1","task_id":"task-1","description":"sleep 100","output_path":"/tmp/out"}}}));
        wire.event(12, "tool-result", json!({"turn_id":"t","request_id":"r","results":[{"call_id":"call-1","result":{"content":"Command moved to the background (task task-1)."}}]}));
        wire.event(13, "tool-use", json!({"turn_id":"t","request_id":"r2","call":{"id":"call-2","name":"agent","input":{"action":"request","prompt":"Survey the crates\nand report","agentId":"reviewer"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"req-1","sequence":14,"create_at":1,"type":"collaboration","name":"request","data":{"turn_id":"t","request_id":"r2","call_id":"call-2","target":{"kind":"agent","agent_id":"reviewer"},"prompt":"Survey the crates\nand report"}}}));
        wire.event(15, "tool-result", json!({"turn_id":"t","request_id":"r2","results":[{"call_id":"call-2","result":{"content":"{\"status\":\"accepted\"}"}}]}));
        let stop = wire.read("session.exec.stop");
        assert_eq!(stop["params"], json!({"sessionId":"session","branchId":"main","taskId":"task-1"}));
        wire.reply(&stop, json!({}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ex-2","sequence":16,"create_at":1,"type":"exec","name":"ended","data":{"task_id":"task-1","status":"cancelled","result":{"content":"stopped by the user"}}}}));
        delivery.recv_timeout(PATIENCE).unwrap();
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-1","sequence":17,"create_at":1,"type":"collaboration","name":"delivered","data":{"op_event_id":"req-1","status":"ok","agent_id":"reviewer","session_id":"member","input_event_id":"member-input"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"req-failed","sequence":18,"create_at":1,"type":"collaboration","name":"request","data":{"turn_id":"t","request_id":"r3","call_id":"call-3","target":{"kind":"agent","agent_id":"missing"},"prompt":"Unavailable delegate"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-failed","sequence":19,"create_at":1,"type":"collaboration","name":"delivered","data":{"op_event_id":"req-failed","status":"error","error":"agent_not_found"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"req-other","sequence":20,"create_at":1,"type":"collaboration","name":"request","data":{"turn_id":"t","request_id":"r4","call_id":"call-4","target":{"kind":"agent","agent_id":"writer"},"prompt":"Check the docs"}}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"del-other","sequence":21,"create_at":1,"type":"collaboration","name":"delivered","data":{"op_event_id":"req-other","status":"ok","agent_id":"writer","session_id":"other-member","input_event_id":"other-input"}}}));
        let past_observations = |wire: &mut Wire, method: &str| loop {
            let request = wire.read_any();
            if request["method"] != "session.attach" {
                assert_eq!(request["method"], method);
                break request;
            }
        };
        let stop = past_observations(wire, "session.stop");
        assert_eq!(stop["params"], json!({"sessionId":"member","branchId":"main","reason":"stopped by the user"}));
        wire.reply(&stop, json!({}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"res-1","sequence":22,"create_at":1,"type":"collaboration","name":"result","data":{"request_event_id":"req-1","agent_id":"reviewer","session_id":"member","text":"","status":"cancelled","reason":"stopped by the user"}}}));
        let submit = past_observations(wire, "session.submit");
        assert_eq!(submit["params"]["sessionId"], "session");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"res-other","sequence":23,"create_at":1,"type":"collaboration","name":"result","data":{"request_event_id":"req-other","agent_id":"writer","session_id":"other-member","text":"Other agent completed normally.","status":"completed"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"go\r");
    terminal.until("Agent: reviewer");
    terminal.write(b"/tasks\r");
    terminal.until("Background tasks");
    terminal.until("sleep 100");
    terminal.until("Survey the crates");
    terminal.write(b"x");
    terminal.until("Cancelled");
    terminal.write(b"\x1b[27u/tasks\r");
    terminal.until("Survey the crates");
    terminal.write(b"\r");
    terminal.until("Agent is starting; no member Session to stop yet");
    deliver.send(()).unwrap();
    terminal.until("Check the docs");
    terminal.write(b"x");
    terminal.until("agent cancelled: stopped by the user");
    terminal.write(b"\x1b[27ucontinue the leader\r");
    terminal.until("agent result: Other agent completed normally.");
    terminal.write(b"/tasks\r");
    terminal.until("Check the docs");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// Switching to full access changes the preset before it allows the calls
/// already waiting, so none of them keeps asking under the old preset.
#[test]
fn tui_approvals_switch_the_preset_and_always_saves_a_command_rule() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[
            "session.permission.get",
            "session.permission.update",
            "permission.rules",
            "permission.set_rules",
        ]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let read = wire.read("session.permission.get");
        assert_eq!(read["params"], json!({"sessionId":"session","branchId":"main"}));
        wire.reply(&read, json!({"version":1,"policy":{"preset":"workspace-write","allowedTools":[],"disallowedTools":[]}}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let read = wire.read("session.permission.get");
        wire.reply(&read, json!({"version":1,"policy":{"preset":"workspace-write","allowedTools":[],"disallowedTools":[]}}));
        let update = wire.read("session.permission.update");
        assert_eq!(update["params"], json!({"sessionId":"session","branchId":"main","expectedVersion":1,"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}));
        wire.reply(&update, json!({"version":2}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"perm-1","sequence":9,"create_at":1,"type":"permission","name":"changed","data":{"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}}}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"review","requestId":"req-1","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"npm test"}},"reason":"shell command"}));
        let rules = wire.read("permission.rules");
        wire.reply(&rules, json!({"revision":"r1","commandRules":[]}));
        let set = wire.read("permission.set_rules");
        assert_eq!(set["params"], json!({"expectedRevision":"r1","commandRules":[{"command":"npm test","match":"exact","decision":"allow"}]}));
        wire.reply(&set, json!({"revision":"r2"}));
        let respond = wire.read("approval.respond");
        assert_eq!(respond["params"], json!({"sessionId":"session","branchId":"review","requestId":"req-1","decision":"allow"}));
        wire.reply(&respond, json!({}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"req-2","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"npm run lint"}},"reason":"shell command"}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"req-3","source":{"kind":"model"},"call":{"name":"write","input":{"path":"notes.md"}},"reason":"file write"}));
        let read = wire.read("session.permission.get");
        wire.reply(&read, json!({"version":2,"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}));
        let mut allowed = Vec::new();
        for _ in 0..2 {
            let respond = wire.read("approval.respond");
            assert_eq!(respond["params"]["decision"], "allow");
            allowed.push(respond["params"]["requestId"].as_str().unwrap().to_owned());
            wire.reply(&respond, json!({}));
        }
        assert_eq!(allowed, ["req-2", "req-3"]);
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-9","sequence":10,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"both allowed"}]},"mode":"queue"}}}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/approvals\r");
    terminal.until("Workspace write");
    terminal.write(b"\x1b[A");
    terminal.write(b"\r");
    terminal.until("Confirm Full Access");
    terminal.write(b"y");
    terminal.until("Tool approvals: Full access");
    terminal.until("Permission required");
    terminal.until("Allow always");
    terminal.write(b"a");
    terminal.until("Rule saved: allow exact `npm test`");
    terminal.until("npm run lint");
    terminal.write(b"\x1b[C\x1b[C\x1b[C\r");
    terminal.until("Confirm Full Access");
    terminal.write(b"y");
    terminal.until("Tool approvals: Full access");
    terminal.until("both allowed");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
    let config: Value =
        serde_json::from_str(&fs::read_to_string(home.0.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["permissionPreset"], "full-access");
    fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.permission.get", "session.permission.update"]);
        let create = wire.read("session.create");
        assert_eq!(create["params"]["permissionPolicy"], json!({"preset":"full-access"}));
        wire.reply(&create, json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let read = wire.read("session.permission.get");
        wire.reply(&read, json!({"version":1,"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("full access");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// The terminal's program status (OSC 7501) follows a run through a
/// permission request and a question to its result, and the record is
/// cleared on `/exit`, in both interface modes. So does the progress bar (OSC 9;4) where it is
/// drawn, which is repeated while it shows and removed on `/exit`.
#[test]
fn tui_reports_the_run_state_to_the_terminal_and_clears_it_on_exit() {
    let report = |pairs: &str| format!("\x1b]7501;{pairs}\x1b\\");
    let (busy, paused, removed) = ("\x1b]9;4;3\x07", "\x1b]9;4;4;100\x07", "\x1b]9;4;0\x07");
    for (fullscreen, drawn) in [(false, true), (true, true), (false, false)] {
        let home = Home::new();
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], "inspect");
            wire.reply(&submit, json!({"event":{}}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"inspect"}]},"mode":"queue"}}}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"ap-1","sequence":10,"create_at":1,"type":"input","name":"applied","data":{"input_id":"in-1","mode":"queue"}}}));
            wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":true}));
            wait_shown.recv_timeout(PATIENCE).unwrap();
            wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"approval","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"ls"}},"reason":"shell command"}));
            let respond = wire.read("approval.respond");
            assert_eq!(respond["params"]["decision"], "allow");
            wire.reply(&respond, json!({}));
            wire.event(11, "tool-call", json!({"turn_id":"turn","request_id":"ask","calls":[{"id":"c1","name":"ask_user","input":{"questions":[{"question":"Where to deploy?","options":[{"label":"production"},{"label":"staging"}]}]}}]}));
            let answer = wire.read("session.question.respond");
            assert_eq!(answer["params"], json!({"sessionId":"session","branchId":"main","requestId":"ask","callId":"c1","answers":[{"questionId":"1","selected":["production"]}]}));
            wire.reply(&answer, json!({}));
            wire.event(12, "tool-result", json!({"turn_id":"turn","request_id":"ask","call_event_id":"event-11","results":[{"call_id":"c1","result":{"content":"production"}}]}));
            wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
            wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":false}));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        command.env("DIM_TERMINAL_PROGRESS", if drawn { "on" } else { "off" });
        let mut terminal = Terminal::launch_tui(command);
        terminal.until(&report("state=idle:app=dim"));
        terminal.until("test / model");
        terminal.write(b"inspect\r");
        terminal.until(&report("state=working:app=dim"));
        if drawn {
            terminal.until(busy);
        }
        shown.send(()).unwrap();
        terminal.until(&report(
            "state=blocked:kind=permission:app=dim:msg=UGVybWlzc2lvbiBuZWVkZWQ6IGV4ZWM=",
        ));
        if drawn {
            terminal.until(paused);
        }
        terminal.until("Permission required");
        if drawn {
            terminal.until(paused);
        }
        terminal.write(b"y");
        terminal.until(&report("state=blocked:kind=question:app=dim"));
        terminal.until("Where to deploy?");
        terminal.write(b"1\r");
        terminal.until(&report("state=done:app=dim"));
        if drawn {
            terminal.until(removed);
        }
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        if drawn {
            terminal.until(removed);
        }
        terminal.until(&report("state=clear"));
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        let transcript = &terminal.transcript;
        assert_eq!(transcript.contains("\x1b[?1049h"), fullscreen);
        for pairs in ["state=idle:app=dim", "state=done:app=dim", "state=clear"] {
            assert_eq!(
                transcript.matches(&report(pairs)).count(),
                1,
                "{fullscreen} {pairs}: {transcript}"
            );
        }
        assert_eq!(
            transcript.rfind("\x1b]7501;"),
            transcript.rfind(&report("state=clear")),
            "{fullscreen}: {transcript}"
        );
        if drawn {
            assert_eq!(
                transcript.matches(removed).count(),
                2,
                "{fullscreen}: {transcript}"
            );
            assert_eq!(
                transcript.rfind("\x1b]9;4;"),
                transcript.rfind(removed),
                "{fullscreen}: {transcript}"
            );
        } else {
            assert!(!transcript.contains("\x1b]9;4;"), "{transcript}");
        }
    }

    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut command = home.command();
    command.env("DIM_TERMINAL_STATUS", "off");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(
        !terminal.transcript.contains("\x1b]7501;"),
        "{}",
        terminal.transcript
    );

    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until(&report("state=idle:app=dim"));
    terminal.until("test / model");
    terminal.signal(libc::SIGTERM);
    terminal.until(&report("state=clear"));
    terminal.wait();
    daemon.join().unwrap();
}

/// `dimcode export` (Timeline export design §7, §13 item 6): the sibling
/// App Server's read-only export, live and after stop, without a daemon.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn export_writes_the_timeline_without_starting_a_daemon() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    let _daemon = OwnedDaemon(socket.clone());
    let run = |args: &[&str]| {
        home.command()
            .arg("--server")
            .arg(&binary)
            .args(args)
            .current_dir(&home.0)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let exec = run(&[
        "--model-config",
        config.to_str().unwrap(),
        "exec",
        "-p",
        "hello",
    ]);
    let stderr = String::from_utf8(exec.stderr).unwrap();
    let session = stderr
        .lines()
        .find_map(|line| line.strip_prefix("session: "))
        .unwrap_or_else(|| panic!("{stderr}"))
        .to_owned();
    let lines = |text: &[u8]| -> Vec<Value> {
        std::str::from_utf8(text)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };

    let live = run(&["export", &session]);
    assert!(
        live.status.success(),
        "{}",
        String::from_utf8_lossy(&live.stderr)
    );
    assert!(live.stderr.is_empty());
    let exported = lines(&live.stdout);
    assert_eq!(exported[0]["kind"], "header");
    assert_eq!(exported[0]["session_id"], session.as_str());
    assert_eq!(exported[0]["sensitive"], true);
    assert_eq!(exported.last().unwrap()["kind"], "end");
    let output = home.0.join("timeline.jsonl");
    let written = run(&["export", &session, "--output", "timeline.jsonl"]);
    assert!(written.status.success());
    assert!(written.stdout.is_empty());
    assert!(String::from_utf8_lossy(&written.stderr).contains("Exported Timeline (sensitive): "));
    assert_eq!(fs::read(&output).unwrap(), live.stdout);
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let refused = run(&["export", &session, "--output", "timeline.jsonl"]);
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(fs::read(&output).unwrap(), live.stdout);
    let failed = run(&["export", "missing-session", "--output", "failed.jsonl"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Session missing-session is not in"));
    assert!(!home.0.join("failed.jsonl").exists());

    assert!(run(&["daemon", "stop"]).status.success());
    assert!(!socket.exists());
    let stopped = run(&["export", &session, "--database", "rust/dimcode.sqlite"]);
    assert!(stopped.status.success());
    let stopped = lines(&stopped.stdout);
    assert_eq!(stopped[0], exported[0]);
    let events = |lines: &[Value]| -> Vec<Value> {
        lines
            .iter()
            .filter(|line| line["kind"] == "event")
            .cloned()
            .collect()
    };
    let (before, after) = (events(&exported), events(&stopped));
    assert!(!before.is_empty());
    assert_eq!(after[..before.len()], before[..]);
    assert!(!socket.exists());
}

#[test]
fn tui_provider_create_recovery_preserves_changed_draft_until_confirmation() {
    let home = Home::new();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.management","provider.create","provider.update","provider.probe"]}));
        let create = wire.read("session.create");
        wire.reply(&create, json!({"sessionId":"session","sequence":0,"model":{"providerId":"test","modelId":"model"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
        let accounts = wire.read("account.providers");
        wire.reply(&accounts, json!([]));
        for _ in 0..2 {
            let listed = wire.read("provider.management");
            wire.reply(&listed, json!({"providers":[],"errors":[]}));
        }
        let probe = wire.read("provider.probe");
        wire.reply(&probe, json!({"models":[]}));
        let create = wire.read("provider.create");
        let id = create["params"]["providerId"].clone();
        let mut entry = json!({"providerId":id,"source":"custom","displayName":"Recovered provider","enabled":true,"version":1,"preferences":{},"connection":{"driverKind":"openai-compatible","auth":{"type":"bearer"},"baseUrl":"https://first.example/v1","credentialConfigured":true}});
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,"message":"Create reply lost"}}));
        let listed = wire.read("provider.management");
        wire.send(json!({"jsonrpc":"2.0","id":listed["id"],"error":{"code":-32000,"message":"Snapshot unavailable"}}));
        for _ in 0..2 {
            let listed = wire.read("provider.management");
            wire.reply(&listed, json!({"providers":[entry],"errors":[]}));
        }
        let probe = wire.read("provider.probe");
        assert_eq!(probe["params"]["providerId"], id);
        assert_eq!(probe["params"]["connection"], json!({"baseUrl":"https://second.example/v1","credential":{"action":"set","value":"second-key"}}));
        wire.reply(&probe, json!({"models":[]}));
        let update = wire.read("provider.update");
        assert_eq!(update["params"]["providerId"], id);
        assert_eq!(update["params"]["expectedVersion"], 1);
        assert_eq!(update["params"]["patch"], probe["params"]["connection"]);
        entry["version"] = json!(2);
        entry["connection"]["baseUrl"] = json!("https://second.example/v1");
        wire.reply(&update, entry.clone());
        let listed = wire.read("provider.management");
        wire.reply(&listed, json!({"providers":[entry],"errors":[]}));
        wait_release.recv_timeout(PATIENCE).unwrap();
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("Ask anything...");
    terminal.write(b"/connect\r");
    terminal.until("Add custom provider");
    terminal.write(b"\rRecovered provider\rhttps://first.example/v1\rfirst-key\r\r");
    terminal.until("Snapshot unavailable");
    terminal.write(b"\x1b[A\x15second-key\x1b[A\x15https://second.example/v1\x1b[B\x1b[B\r");
    terminal.until("Provider changed; review the retained draft");
    terminal.write(b"\r");
    terminal.until("Connected Recovered provider");
    terminal.write(b"\x1b\x1b");
    terminal.until("Ask anything...");
    terminal.write(b"/exit\r");
    let status = terminal.wait();
    release.send(()).unwrap();
    daemon.join().unwrap();
    assert!(status.success());
}

/// `/models` edits the subagent model policy through its own management
/// methods only: a stale save rereads instead of overwriting, cancel writes
/// nothing, and neither the Session's model nor the provider default moves.
#[test]
fn tui_models_picker_edits_only_the_subagent_model_policy() {
    let home = Home::new();
    let (calls, observed) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        let mut revision = 1;
        let mut policy = json!({"mode":"agent"});
        let mut conflicted = false;
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            line.clear();
            calls.send(request.clone()).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "hello" => json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,
                    "daemonProfile":{"kind":"product"},"features":["session.create","session.history",
                    "provider.list","provider.catalog","collaboration.model_policy",
                    "collaboration.set_model_policy"]}),
                "session.create" => json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}),
                "session.history" => json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                "session.idle" => json!({"sequence":8}),
                "provider.list" => json!({"providers":[{"providerId":"test","displayName":"Test","enabled":true}]}),
                "provider.catalog" => json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                    {"id":"model","name":"Model","capabilities":dim_agent_types::ModelCapabilities::default()},
                    {"id":"other","name":"Other","capabilities":dim_agent_types::ModelCapabilities::default()}]}}),
                "account.providers" => json!([]),
                "collaboration.model_policy" => json!({"revision":revision.to_string(),"policy":policy}),
                "collaboration.set_model_policy" => {
                    if !conflicted {
                        conflicted = true;
                        revision += 1;
                    }
                    if request["params"]["expectedRevision"].as_str() != Some(&*revision.to_string()) {
                        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,
                            "message":"conflict","data":{"appError":"collaboration_model_policy_conflict"}}}));
                        continue;
                    }
                    revision += 1;
                    policy = request["params"]["policy"].clone();
                    json!({"revision":revision.to_string()})
                }
                _ => json!({}),
            };
            wire.reply(&request, result);
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/models\r");
    terminal.until("Subagent models: Agent choice");
    terminal.write(b"Subagent\r");
    terminal.until("Inherit requesting branch");
    terminal.until("Existing members keep their models.");
    terminal.raw(b"\x1b[B\r");
    terminal.until("Subagent model policy changed elsewhere");
    terminal.write(b"\r");
    thread::sleep(Duration::from_millis(300));
    terminal.write(b"\r");
    terminal.until("Subagent models: Inherit requesting branch");
    terminal.until("Subagent model policy saved");
    terminal.write(b"\r");
    terminal.until("Fixed model");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(300));
    terminal.write(b"\r");
    terminal.until("Fixed model");
    terminal.raw(b"\x1b[B\r");
    terminal.until("Fixed subagent model");
    terminal.write(b"other\r");
    terminal.until("Subagent models: Test / other");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
    let calls: Vec<Value> = observed.try_iter().collect();
    let methods: Vec<&str> = calls
        .iter()
        .map(|call| call["method"].as_str().unwrap())
        .collect();
    for forbidden in [
        "session.model.update",
        "provider.select",
        "session.agent.update",
    ] {
        assert!(!methods.contains(&forbidden), "{methods:?}");
    }
    assert_eq!(
        methods.iter().filter(|m| **m == "session.create").count(),
        1
    );
    let saves: Vec<&Value> = calls
        .iter()
        .filter(|call| call["method"] == "collaboration.set_model_policy")
        .map(|call| &call["params"])
        .collect();
    assert_eq!(
        saves,
        [
            &json!({"expectedRevision":"1","policy":{"mode":"inherit"}}),
            &json!({"expectedRevision":"2","policy":{"mode":"inherit"}}),
            &json!({"expectedRevision":"3","policy":{"mode":"fixed","model":{"providerId":"test","modelId":"other"}}}),
        ]
    );
}

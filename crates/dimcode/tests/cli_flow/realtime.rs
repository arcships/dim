//! `dimcode realtime`: one session through the product — the built binary,
//! the daemon it attaches to, the built-in adapter and a WebSocket stub —
//! and the exit codes only the binary can show.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as WsRequest, Response as WsResponse,
};

const CREDENTIAL: &str = "realtime-test-credential";
const MODEL: &str = "stepaudio-2.5-realtime";

/// tungstenite's `Callback` hands back an `ErrorResponse` of its own size,
/// which is not ours to box.
type Handshake = std::result::Result<WsResponse, http::Response<Option<String>>>;

/// A WebSocket stub that checks the relay's handshake and its one text frame,
/// answers with one event, and then waits for the relay's close.
fn stub(listener: std::net::TcpListener) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        listener.set_nonblocking(true).unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            #[allow(clippy::result_large_err)]
            fn checked(request: &WsRequest, response: WsResponse) -> Handshake {
                assert_eq!(
                    request.uri().path_and_query().unwrap().as_str(),
                    format!("/v1/realtime?model={MODEL}")
                );
                assert_eq!(
                    request.headers().get("authorization").unwrap(),
                    format!("Bearer {CREDENTIAL}").as_str()
                );
                Ok(response)
            }
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(stream, checked)
                .await
                .unwrap();
            let text = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = text else {
                panic!("a stdin line is a text frame")
            };
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["type"], "session.update");
            assert_eq!(value["session"]["instructions"], "hi");
            socket
                .send(Message::Text(r#"{"type":"session.created"}"#.into()))
                .await
                .unwrap();
            while let Some(message) = socket.next().await {
                if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                    return;
                }
            }
        });
    })
}

/// A raw client on the daemon's socket, for the setup the CLI has no command
/// for: the provider record that points the bundled model at the stub.
struct Attach {
    stream: BufReader<UnixStream>,
    id: u64,
}

impl Attach {
    fn open(socket: &std::path::Path) -> Self {
        let deadline = Instant::now() + PATIENCE;
        loop {
            match UnixStream::connect(socket) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(PATIENCE)).unwrap();
                    return Self {
                        stream: BufReader::new(stream),
                        id: 0,
                    };
                }
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("{error}"),
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        let mut bytes = serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .unwrap();
        bytes.push(b'\n');
        self.stream.get_mut().write_all(&bytes).unwrap();
        loop {
            let mut line = String::new();
            assert_ne!(
                self.stream.read_line(&mut line).unwrap(),
                0,
                "the daemon closed before the reply to {method}"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    }
}

/// Waits for a client that has to exit, so a hung one fails instead of
/// blocking the run.
fn waited(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "the CLI did not exit");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_realtime_session_relays_frames_and_events_through_the_product() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stub = stub(listener);

    // The product daemon, started as the CLI starts it.
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    daemon_cleanup::mark_test_server(&home.0);
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_dimcode"))
        .arg("app-server")
        .arg("--unix")
        .arg(&socket)
        .args(["--no-ws", "--home"])
        .arg(home.0.join("rust"))
        .arg("--product-home")
        .arg(&home.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    while !socket.exists() {
        assert!(
            daemon.try_wait().unwrap().is_none(),
            "the daemon exited before it bound its socket"
        );
        assert!(
            Instant::now() < deadline,
            "the daemon never bound its socket"
        );
        thread::sleep(Duration::from_millis(20));
    }

    // Point the bundled realtime model at the stub, as a user does.
    let mut client = Attach::open(&socket);
    let hello = client.request(
        "hello",
        json!({"protocolVersion": 3, "clientKind": "other"}),
    );
    assert!(hello.get("error").is_none(), "{hello}");
    let list = client.request("provider.list", json!({}));
    let provider = list["result"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == "step-plan")
        .unwrap_or_else(|| panic!("no step-plan provider: {list}"))
        .clone();
    let update = client.request(
        "provider.update",
        json!({
            "providerId": "step-plan",
            "expectedVersion": provider["version"],
            "patch": {
                "baseUrl": format!("http://{address}/v1"),
                "enabled": true,
                "credential": {"action": "set", "value": CREDENTIAL},
            },
        }),
    );
    assert!(update.get("error").is_none(), "{update}");

    // The real CLI attaches to that daemon. Stdin stays open until the stub's
    // answer is on stdout, so the event and the EOF close cannot race.
    let mut child = home
        .command()
        .args(["realtime", "--model", "step-plan/stepaudio-2.5-realtime"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"{\"type\":\"session.update\",\"session\":{\"instructions\":\"hi\"}}\n")
        .unwrap();
    stdin.flush().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (lines, printed) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if lines.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let first = match printed.recv_timeout(PATIENCE) {
        Ok(line) => line,
        Err(error) => {
            // A CLI that failed instead of printing exits here, and its
            // stderr is what explains it.
            let status = waited(&mut child);
            let mut stderr = String::new();
            let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
            panic!(
                "the stub's event was not printed ({error:?}); the CLI exited {status:?}: {stderr}"
            );
        }
    };
    assert_eq!(first, r#"{"type":"session.created"}"#);
    drop(stdin);
    let mut rest = Vec::new();
    while let Ok(line) = printed.recv_timeout(PATIENCE) {
        rest.push(line);
    }
    let status = waited(&mut child);
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "the CLI exited {status:?}: {stderr}");
    reader.join().unwrap();
    assert_eq!(rest.len(), 1, "{rest:?}");
    let ended: Value = serde_json::from_str(&rest[0]).unwrap();
    assert_eq!(ended["reason"], "closed", "{ended}");
    assert!(
        ended["connectionId"].as_str().unwrap().starts_with("rc_"),
        "{ended}"
    );
    stub.join().unwrap();
    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
fn an_unsupported_operation_exits_one() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let open = wire.read("modality.realtime.open");
        assert_eq!(open["params"]["operation"], "nope");
        wire.send(json!({
            "jsonrpc": "2.0",
            "id": open["id"],
            "error": {
                "code": -32000,
                "message": "Unknown realtime operation nope",
                "data": {"appError": "modality_unknown_operation"},
            },
        }));
    });
    let output = home
        .command()
        .args(["realtime", "--operation", "nope"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown realtime operation nope"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn an_interrupt_before_the_open_reply_exits_130() {
    let home = Home::new();
    let (opened, open_seen) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let open = wire.read("modality.realtime.open");
        assert_eq!(open["params"]["operation"], "voice");
        opened.send(()).unwrap();
        // Holding the reply leaves the interrupt as the only end.
        assert!(
            wire.trailing_request().is_none(),
            "the CLI sent after the interrupt"
        );
    });
    let mut child = home
        .command()
        .args(["realtime"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    open_seen
        .recv_timeout(PATIENCE)
        .expect("the open request reached the daemon");
    assert_eq!(
        unsafe { libc::kill(child.id() as i32, libc::SIGINT) },
        0,
        "signal the CLI"
    );
    let status = waited(&mut child);
    assert_eq!(status.code(), Some(130), "the CLI exited {status:?}");
    daemon.join().unwrap();
}

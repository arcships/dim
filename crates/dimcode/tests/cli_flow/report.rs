//! `/report` over the product: the daemon names the build it runs in the
//! report it generates, and names a client whose label is another one.
use super::*;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;

/// A raw client on the daemon's socket, so the flow can send the reported
/// error itself and choose the label the client claims.
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

/// Starts the product daemon on `home` and waits for its socket.
fn daemon(home: &Home) -> (Child, std::path::PathBuf) {
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
    (daemon, socket)
}

/// The report body an observed error generates, and what it names as the
/// client's label; `None` when the client's label matched the daemon's.
fn report(client: &mut Attach, claimed: &str) -> (String, Option<Value>) {
    let observed = json!({
        "operation": "session_read",
        "code": "session_closed",
        "observedAt": "2026-11-10T00:00:00Z",
        "clientVersion": claimed,
    });
    let prepared = client.request(
        "report.prepare",
        json!({"error": {"kind": "observed", "observed": observed}}),
    );
    assert!(prepared.get("error").is_none(), "{prepared}");
    let report_id = prepared["result"]["reportId"].clone();
    let plan = client.request(
        "report.plan",
        json!({
            "reportId": report_id,
            "generation": {"kind": "local"},
            "redact": false,
        }),
    );
    assert!(plan.get("error").is_none(), "{plan}");
    let generated = client.request(
        "report.generate",
        json!({
            "reportId": report_id,
            "planId": plan["result"]["planId"],
        }),
    );
    assert!(generated.get("error").is_none(), "{generated}");
    let body = generated["result"]["body"].as_str().unwrap().to_owned();
    let client_line = generated["result"]["files"]["diagnostics.json"]
        .as_str()
        .map(|diagnostics| serde_json::from_str::<Value>(diagnostics).unwrap())
        .and_then(|diagnostics| {
            diagnostics["evidence"]["process"]
                .get("clientVersion")
                .cloned()
        });
    (body, client_line)
}

#[test]
fn a_report_names_the_build_the_binary_prints_and_a_client_of_another_build() {
    let home = Home::new();
    let label = printed_label(&home);
    let (mut daemon, socket) = daemon(&home);
    let mut client = Attach::open(&socket);
    let hello = client.request(
        "hello",
        json!({"protocolVersion": 4, "clientKind": "other"}),
    );
    assert!(hello.get("error").is_none(), "{hello}");
    assert!(
        hello["result"]["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|feature| feature == "report.prepare"),
        "the daemon offers the process report entry: {hello}"
    );

    let (body, client_line) = report(&mut client, &label);
    assert!(
        body.contains(&format!("- Dim {label} (protocol 4)\n")),
        "the Dim line names the build the binary prints: {body}"
    );
    assert_eq!(client_line, None, "a client of this build adds no line");

    let (body, client_line) = report(&mut client, "0.0.0-alpha.1-source+ffffffff");
    assert!(
        body.contains("- Client 0.0.0-alpha.1-source+ffffffff\n"),
        "a client of another build is named in the report: {body}"
    );
    assert_eq!(
        client_line,
        Some(json!("0.0.0-alpha.1-source+ffffffff")),
        "the diagnostics carry the client's label beside the daemon's"
    );

    daemon.kill().unwrap();
    daemon.wait().unwrap();
}

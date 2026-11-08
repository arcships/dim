//! `realtime [--operation voice] [--model PROVIDER/MODEL]`: one realtime
//! session over this connection (Modality A.3, §3.3).
//!
//! The session's events and its end print as one JSON line each; stdin lines
//! are sent as text frames, so the first line a client pipes in can be the
//! provider session's `session.update`. Stdin EOF or Ctrl+C closes the
//! session; a direct transport prints the connection and holds instead.
use crate::client::{Client, Message};
use crate::plugins::Signals;
use crate::{Result, args::Options};
use dim_agent_types::ModelId;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

/// `PROVIDER/MODEL`, as every CLI model reference is written.
fn model_id(reference: &str) -> Result<ModelId> {
    let Some((provider_id, model_id)) = reference.split_once('/') else {
        return Err("--model must be PROVIDER/MODEL".into());
    };
    if provider_id.is_empty() || model_id.is_empty() {
        return Err("--model must be PROVIDER/MODEL".into());
    }
    Ok(ModelId {
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
    })
}

/// One line of the session's output, flushed so a pipe sees it at once.
fn print_line(output: &mut impl std::io::Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

/// Ends the session the way the client asks, and returns the request's id: the
/// reply is read in the loop, so `not_found` (the session had already ended)
/// is not this command's error.
async fn send_close(client: &mut Client, connection_id: &str) -> Result<u64> {
    client
        .send(
            "modality.realtime.close",
            json!({"connectionId": connection_id}),
        )
        .await
}

/// What one loop iteration waits on. The select only records the step, so the
/// client is free for the send or the close the step then performs.
enum Step {
    Signal,
    Line(std::io::Result<Option<String>>),
    Message(dim_client::Result<Message>),
}

/// How long the command keeps reading after it asks to close: the events in
/// flight and the session's `ended` are still worth printing.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

pub async fn run(client: &mut Client, options: &Options, operation: &str) -> Result<i32> {
    run_with(
        client,
        options,
        operation,
        tokio::io::stdin(),
        std::io::stdout(),
    )
    .await
}

/// [`run`] over the streams a caller gives it: the product passes the
/// process's own stdin and stdout, and a test a piped session and a buffer.
pub(crate) async fn run_with<R, W>(
    client: &mut Client,
    options: &Options,
    operation: &str,
    input: R,
    mut output: W,
) -> Result<i32>
where
    R: tokio::io::AsyncRead + Unpin,
    W: std::io::Write,
{
    let mut signals = Signals::listen()?;
    let mut params = json!({"operation": operation, "params": {}});
    if let Some(reference) = &options.model {
        let model = model_id(reference)?;
        params["model"] = json!({"providerId": model.provider_id, "modelId": model.model_id});
    }
    let id = client.send("modality.realtime.open", params).await?;
    let result = tokio::select! {
        // An error reply is the command's own error: the client prints it and
        // this exits 1.
        reply = client.response(id) => reply?,
        // Before the open reply, Ctrl+C is the shell's 130.
        _ = signals.received() => return Ok(130),
    };
    let connection_id = result["connectionId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let direct = result["transport"] == "direct";
    if direct {
        // The caller talks to the provider's own transport from here.
        print_line(
            &mut output,
            result.get("connection").unwrap_or(&Value::Null),
        )?;
    }
    let mut stdin = BufReader::new(input).lines();
    // The id of the close request this command sent; stdin is spent from then
    // on and only the session's end finishes the command.
    let mut closing: Option<u64> = None;
    loop {
        let step = match closing {
            Some(_) => tokio::select! {
                _ = signals.received() => return Ok(0),
                message = client.next() => Step::Message(message),
                _ = tokio::time::sleep(CLOSE_WAIT) => return Ok(0),
            },
            None => tokio::select! {
                _ = signals.received() => Step::Signal,
                line = stdin.next_line() => Step::Line(line),
                message = client.next() => Step::Message(message),
            },
        };
        match step {
            Step::Signal => closing = Some(send_close(client, &connection_id).await?),
            Step::Line(line) => match line? {
                // Stdin EOF closes the session.
                None => closing = Some(send_close(client, &connection_id).await?),
                Some(line) => {
                    if direct || line.trim().is_empty() {
                        continue;
                    }
                    let data: Value = serde_json::from_str(&line)
                        .map_err(|_| "Each stdin line must be one JSON value")?;
                    client
                        .send(
                            "modality.realtime.send",
                            json!({
                                "connectionId": connection_id,
                                "frame": {"kind": "text", "data": data},
                            }),
                        )
                        .await?;
                }
            },
            Step::Message(message) => match message? {
                Message::Notice { method, params } => match method.as_str() {
                    "modality.realtime.event" => print_line(&mut output, &params["event"])?,
                    "modality.realtime.ended" => {
                        print_line(&mut output, &params)?;
                        return Ok(0);
                    }
                    _ => {}
                },
                Message::Response { id, result } => {
                    if Some(id) == closing {
                        // `not_found`: the session had already ended.
                        if result.is_err() {
                            return Ok(0);
                        }
                    } else if let Err(error) = result {
                        // A send's reply: an error ends the command.
                        eprintln!("{}", error.message);
                        return Ok(1);
                    }
                }
            },
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    //! The command against a scripted daemon: the CLI still speaks only the
    //! protocol, so these tests link no App Server. The product composition —
    //! the built binary, the real daemon and the built-in adapter — is proven
    //! in `crates/dimcode/tests/cli_flow/realtime.rs`.
    use super::*;
    use dim_client::{ConnectionConfig, Server};
    use std::io::{BufRead, BufReader, Write as _};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use tokio::io::AsyncWriteExt;

    /// How long one scripted step waits before it fails instead of hanging.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// The daemon's hello, as a compatible server sends it.
    fn hello_result() -> Value {
        json!({
            "instanceId": "scripted",
            "serverVersion": "test",
            "protocolVersion": 3,
            "features": ["session.create", "session.history"],
            "daemonProfile": {"kind": "product"},
        })
    }

    /// One scripted daemon and the home its socket sits in.
    struct Script {
        home: tempfile::TempDir,
        socket: PathBuf,
        daemon: Option<std::thread::JoinHandle<()>>,
    }

    impl Script {
        /// Binds the socket, then runs `flow` on the one connection the CLI
        /// opens. Binding first means a client can connect at once.
        fn start(flow: impl FnOnce(&mut Wire) + Send + 'static) -> Self {
            let home = tempfile::tempdir().unwrap();
            let socket = home.path().join("daemon.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
            let daemon = std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("the CLI connected");
                stream
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                flow(&mut Wire(BufReader::new(stream)));
            });
            Self {
                home,
                socket,
                daemon: Some(daemon),
            }
        }

        /// A connected client, past the handshake every CLI action starts with.
        async fn client(&self) -> Client {
            let config = ConnectionConfig {
                endpoint: self.socket.clone(),
                server: Server {
                    program: PathBuf::from("dimcode"),
                    embedded: false,
                },
                profile: None,
                client_kind: "cli",
                client_name: "dimcode",
            };
            let mut client = Client::open(&config, false).await.unwrap();
            client.hello().await.unwrap();
            client
        }

        fn home(&self) -> &Path {
            self.home.path()
        }

        /// Surfaces a scripted assertion that failed in the daemon's thread.
        fn join(&mut self) {
            if let Some(daemon) = self.daemon.take() {
                daemon.join().unwrap();
            }
        }
    }

    /// The daemon's side of the connection: JSON-RPC frames, one line each.
    struct Wire(BufReader<std::os::unix::net::UnixStream>);

    impl Wire {
        fn frame(&mut self) -> Value {
            self.next_frame().expect("the CLI closed before a frame")
        }

        /// The next request, or `None` when the CLI closed first.
        fn next_frame(&mut self) -> Option<Value> {
            let deadline = Instant::now() + PATIENCE;
            let mut line = String::new();
            loop {
                match self.0.read_line(&mut line) {
                    Ok(0) => return None,
                    Ok(_) => return Some(serde_json::from_str(&line).unwrap()),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        assert!(
                            Instant::now() < deadline,
                            "the CLI sent nothing within {PATIENCE:?}"
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                        return None;
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        }

        fn reply(&mut self, request: &Value, result: Value) {
            self.send(json!({"jsonrpc": "2.0", "id": request["id"], "result": result}));
        }

        fn error(&mut self, request: &Value, message: &str) {
            self.send(json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": {"code": -32000, "message": message},
            }));
        }

        fn notice(&mut self, method: &str, params: Value) {
            self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
        }

        fn send(&mut self, value: Value) {
            let written =
                writeln!(self.0.get_mut(), "{value}").and_then(|()| self.0.get_mut().flush());
            if let Err(error) = written {
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                    ),
                    "{error}"
                );
            }
        }
    }

    fn options(home: &Path, model: Option<&str>) -> Options {
        let mut arguments = vec![
            "--home".to_owned(),
            home.to_str().unwrap().to_owned(),
            "realtime".to_owned(),
        ];
        if let Some(model) = model {
            arguments.extend(["--model".to_owned(), model.to_owned()]);
        }
        Options::parse(arguments.into_iter()).unwrap()
    }

    /// The command's stdout as JSON lines.
    fn lines(output: Vec<u8>) -> Vec<Value> {
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_prints_events_and_closes_on_eof() {
        // The frame reaches the daemon before stdin ends, so the close the
        // test asserts is the EOF's and not a race.
        let (sent, sent_rx) = std::sync::mpsc::channel();
        let mut script = Script::start(move |wire| {
            let hello = wire.frame();
            assert_eq!(hello["method"], "hello");
            wire.reply(&hello, hello_result());
            let open = wire.frame();
            assert_eq!(open["method"], "modality.realtime.open");
            assert_eq!(open["params"]["operation"], "voice");
            assert_eq!(open["params"]["params"], json!({}));
            assert_eq!(
                open["params"]["model"],
                json!({"providerId": "step-plan", "modelId": "stepaudio-2.5-realtime"})
            );
            wire.reply(&open, json!({"connectionId": "rc_1", "transport": "relay"}));
            let send = wire.frame();
            assert_eq!(send["method"], "modality.realtime.send");
            assert_eq!(send["params"]["connectionId"], "rc_1");
            assert_eq!(
                send["params"]["frame"],
                json!({"kind": "text", "data": {"type": "session.update", "session": {"instructions": "hi"}}})
            );
            wire.reply(&send, json!({}));
            sent.send(()).unwrap();
            wire.notice(
                "modality.realtime.event",
                json!({"connectionId": "rc_1", "event": {"type": "session.created"}}),
            );
            wire.notice(
                "modality.realtime.event",
                json!({"connectionId": "rc_1", "event": {"type": "response.text.delta", "delta": "hi"}}),
            );
            // Stdin's EOF closes the session.
            let close = wire.frame();
            assert_eq!(close["method"], "modality.realtime.close");
            assert_eq!(close["params"]["connectionId"], "rc_1");
            wire.reply(&close, json!({}));
            wire.notice(
                "modality.realtime.ended",
                json!({"connectionId": "rc_1", "reason": "closed"}),
            );
        });
        let mut client = script.client().await;
        let (input, mut feed) = tokio::io::duplex(4096);
        feed.write_all(b"{\"type\":\"session.update\",\"session\":{\"instructions\":\"hi\"}}\n")
            .await
            .unwrap();
        let eof = async move {
            tokio::task::spawn_blocking(move || {
                sent_rx
                    .recv_timeout(PATIENCE)
                    .expect("the frame reached the daemon")
            })
            .await
            .unwrap();
            drop(feed);
        };
        let mut output = Vec::new();
        let options = options(script.home(), Some("step-plan/stepaudio-2.5-realtime"));
        let (code, ()) = tokio::join!(
            run_with(&mut client, &options, "voice", input, &mut output),
            eof
        );
        let code = code.unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            lines(output),
            vec![
                json!({"type": "session.created"}),
                json!({"type": "response.text.delta", "delta": "hi"}),
                json!({"connectionId": "rc_1", "reason": "closed"}),
            ]
        );
        script.join();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_error_reply_ends_the_command() {
        let mut script = Script::start(|wire| {
            let hello = wire.frame();
            wire.reply(&hello, hello_result());
            let open = wire.frame();
            assert_eq!(open["method"], "modality.realtime.open");
            wire.error(&open, "No available adapter serves realtime.voice");
            // Nothing follows an error reply.
            assert!(
                wire.next_frame().is_none(),
                "the command sent after an error reply"
            );
        });
        let mut client = script.client().await;
        let mut output = Vec::new();
        let options = options(script.home(), None);
        // `run_cli` prints this error and exits 1.
        let error = run_with(&mut client, &options, "voice", &b""[..], &mut output)
            .await
            .expect_err("an error reply is the command's error");
        assert!(
            error
                .to_string()
                .contains("No available adapter serves realtime.voice"),
            "{error}"
        );
        assert!(output.is_empty());
        drop(client);
        script.join();
    }
}

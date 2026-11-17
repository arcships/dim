#![cfg(unix)]

use dim_acp::{AcpGateway, Io, auth::Login};
use dim_client::{Client, ConnectionConfig, DaemonProfile, Server};
use rutis::host::CtxHost;
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UnixListener},
    sync::oneshot,
};

async fn frame(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
async fn send(writer: &mut (impl AsyncWriteExt + Unpin), value: Value) {
    writer
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
    writer.flush().await.unwrap();
}

async fn browser_flow(cancel: bool, eof: bool) {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = home.path().join("daemon.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let redirect = format!("http://127.0.0.1:{}/callback", address.port());
    let (started, ready) = oneshot::channel();
    let callback = format!("{redirect}?code=fixture&state=owned-attempt");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello = frame(&mut read).await;
        send(&mut write,json!({"jsonrpc":"2.0","id":hello["id"],"result":{"instanceId":"fixture","serverVersion":"fixture","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.input_lookup","account.providers"]}})).await;
        let providers = frame(&mut read).await;
        assert_eq!(providers["method"], "account.providers");
        send(&mut write,json!({"jsonrpc":"2.0","id":providers["id"],"result":[{"providerId":"managed","displayName":"Fixture","loginMethods":["browser"]}]})).await;
        for _ in 0..2 {
            let status = frame(&mut read).await;
            assert_eq!(status["method"], "account.status");
            send(
                &mut write,
                json!({"jsonrpc":"2.0","id":status["id"],"result":{"account":null}}),
            )
            .await;
        }
        let begin = frame(&mut read).await;
        assert_eq!(begin["method"], "account.login.browser.begin");
        assert_eq!(begin["params"]["providerId"], "managed");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        send(&mut write,json!({"jsonrpc":"2.0","id":begin["id"],"result":{"attemptId":"owned-attempt","authorizationUrl":"https://fixture.invalid/login","expiresAt":now+30_000}})).await;
        started.send(()).unwrap();
        let complete = frame(&mut read).await;
        assert_eq!(complete["params"]["attemptId"], "owned-attempt");
        if cancel {
            assert_eq!(complete["method"], "account.login.cancel");
        } else {
            assert_eq!(complete["method"], "account.login.browser.complete");
            assert_eq!(complete["params"]["callbackUrl"], callback);
        }
        send(
            &mut write,
            json!({"jsonrpc":"2.0","id":complete["id"],"result":{}}),
        )
        .await;
        if !cancel {
            let status = frame(&mut read).await;
            assert_eq!(status["method"], "account.status");
            send(&mut write,json!({"jsonrpc":"2.0","id":status["id"],"result":{"account":{"displayName":"fixture"}}})).await;
        }
        let mut eof = String::new();
        read.read_line(&mut eof).await.unwrap();
        assert!(eof.is_empty());
    });
    let client = Client::connect(&ConnectionConfig {
        endpoint,
        server: Server {
            program: "unused".into(),
            embedded: false,
        },
        profile: Some(DaemonProfile::Product),
        client_kind: "other",
        client_name: "AcpGateway",
        protocol_version: 3,
    })
    .await
    .unwrap();
    let (peer, transport) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(transport);
    let (gateway, finished) = AcpGateway::new(
        client,
        Io {
            reader: Box::new(reader),
            writer: Box::new(writer),
        },
        None,
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    let gateway = gateway.login(Login {
        directory: "https://fixture.invalid".into(),
        redirect_uri: redirect,
        open_browser: Some(|_| true),
        terminal_args: None,
    });
    let root = rutis::Ctx::root().unwrap();
    let mounted = root.plugin(gateway);
    (&mounted).await.unwrap();
    let (read, mut write) = tokio::io::split(peer);
    let mut read = BufReader::new(read);
    send(&mut write,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})).await;
    let initialize = frame(&mut read).await;
    let method = &initialize["result"]["authMethods"][0]["id"];
    send(
        &mut write,
        json!({"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"methodId":method}}),
    )
    .await;
    ready.await.unwrap();
    if eof {
        write.shutdown().await.unwrap();
    } else if cancel {
        send(
            &mut write,
            json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":2}}),
        )
        .await;
    } else {
        let mut callback = TcpStream::connect(address).await.unwrap();
        callback.write_all(b"GET /callback?code=fixture&state=owned-attempt HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
    }
    if !eof {
        let response = frame(&mut read).await;
        assert_eq!(response["id"], 2, "{response}");
        if cancel {
            assert_eq!(response["error"]["code"], -32800, "{response}");
        } else {
            assert_eq!(response["result"], json!({}), "{response}");
        }
        write.shutdown().await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), finished)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    mounted.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn browser_auth_uses_the_owned_callback_and_native_exchange() {
    browser_flow(false, false).await;
}
#[tokio::test]
async fn cancelling_browser_auth_cancels_only_the_owned_pending_attempt() {
    browser_flow(true, false).await;
}

#[tokio::test]
async fn eof_releases_callback_and_cancels_the_owned_pending_login_before_rpc_closes() {
    browser_flow(true, true).await;
}

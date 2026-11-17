#![cfg(unix)]

use dim_client::{Client, ConnectionConfig, DaemonProfile, Message, Server, callback::Capture};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UnixListener},
    sync::oneshot,
};

#[tokio::test]
async fn an_owned_connection_preserves_identity_responses_and_partial_notifications() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = home.path().join("daemon.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (partial, written) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read).lines();
        let hello: Value = serde_json::from_str(&read.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(hello["params"]["clientKind"], "other");
        assert_eq!(hello["params"]["clientName"], "AcpGateway");
        let reply = json!({"jsonrpc":"2.0","id":hello["id"],"result":{
            "instanceId":"fixture","serverVersion":"fixture","protocolVersion":4,
            "daemonProfile":{"kind":"product"},"features":["session.create","session.history"]
        }});
        write
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
        let first: Value = serde_json::from_str(&read.next_line().await.unwrap().unwrap()).unwrap();
        let second: Value =
            serde_json::from_str(&read.next_line().await.unwrap().unwrap()).unwrap();
        let replies = format!(
            "{}\n{}\n",
            json!({"jsonrpc":"2.0","id":second["id"],"result":null}),
            json!({"jsonrpc":"2.0","id":first["id"],"result":{"order":1}})
        );
        write.write_all(replies.as_bytes()).await.unwrap();
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session.notice\",\"params\":")
            .await
            .unwrap();
        partial.send(()).unwrap();
        resumed.await.unwrap();
        write
            .write_all(b"{\"text\":\"complete\"}}\n")
            .await
            .unwrap();
        let missing: Value =
            serde_json::from_str(&read.next_line().await.unwrap().unwrap()).unwrap();
        write
            .write_all(format!("{}\n", json!({"jsonrpc":"2.0","id":missing["id"]})).as_bytes())
            .await
            .unwrap();
    });
    let config = ConnectionConfig {
        endpoint,
        server: Server {
            program: "unused-on-unix".into(),
            embedded: false,
        },
        profile: Some(DaemonProfile::Product),
        client_kind: "other",
        client_name: "AcpGateway",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    assert_eq!(client.connection_config().endpoint, config.endpoint);
    let first = client.send("session.first", json!({})).await.unwrap();
    let second = client.send("session.second", json!({})).await.unwrap();
    assert_eq!(client.response(first).await.unwrap(), json!({"order":1}));
    assert!(
        matches!(client.next().await.unwrap(), Message::Response { id, result: Ok(Value::Null) } if id == second)
    );
    written.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), client.next())
            .await
            .is_err()
    );
    resume.send(()).unwrap();
    assert!(
        matches!(client.next().await.unwrap(), Message::Notice { method, params } if method == "session.notice" && params == json!({"text":"complete"}))
    );
    assert_eq!(
        client
            .rpc("session.missing", json!({}))
            .await
            .unwrap_err()
            .to_string(),
        "Invalid App Server response identity"
    );
    server.await.unwrap();
    client.wait_closed().await.unwrap();
}

#[tokio::test]
async fn loopback_capture_rejects_other_paths_without_consuming_the_callback() {
    assert!(
        Capture::bind("https://example.test/callback")
            .await
            .is_none()
    );
    let reserved = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let redirect = format!("http://127.0.0.1:{}/callback", address.port());
    let capture = Capture::bind(&redirect).await.unwrap();
    let expected = format!("{redirect}?code=fixture");
    let accepted = tokio::spawn(async move {
        let (url, mut stream) = capture.receive().await.unwrap();
        assert_eq!(url, expected);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    for (path, status) in [("/unrelated", "404"), ("/callback?code=fixture", "200")] {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{response}"
        );
    }
    accepted.await.unwrap();
}

#[test]
fn conversation_windows_keep_atomic_pages_and_their_raw_cursors() {
    use dim_client::{
        window::Window,
        wire::{DisplayPage, EventContent, HistoryWindow},
    };
    let event = |at| json!({"id":format!("event-{at}"),"sequence":at,"create_at":at,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"message"}]}}});
    let mut window = Window::default();
    let params = window.params_with_display("session", "main", true, true);
    assert_eq!(params["limit"], 32);
    assert_eq!(params["transcript"], true);
    assert!(params.get("latestTurn").is_none());
    let empty: HistoryWindow = serde_json::from_value(
        json!({"events":[],"through":302,"nextAfter":302,"nextBefore":302,"done":false}),
    )
    .unwrap();
    assert!(!window.push(empty).unwrap());
    assert_eq!(
        window.params_with_display("session", "main", true, true)["limit"],
        64
    );
    let page: HistoryWindow = serde_json::from_value(json!({"events":(1..=301).map(event).collect::<Vec<_>>(),"through":302,"nextAfter":301,"nextBefore":1,"done":true})).unwrap();
    assert!(window.push(page).unwrap());
    assert_eq!(
        window.take().len(),
        301,
        "one oversized atomic page stays complete"
    );
    assert!(window.before.is_none());
    let mut window = Window::earlier(450, 449);
    let newest: HistoryWindow = serde_json::from_value(json!({"events":(351..=449).map(event).collect::<Vec<_>>(),"through":449,"nextAfter":449,"nextBefore":301,"done":false})).unwrap();
    assert!(!window.push(newest).unwrap());
    let older: HistoryWindow = serde_json::from_value(json!({"events":(1..=300).map(event).collect::<Vec<_>>(),"through":449,"nextAfter":300,"nextBefore":1,"done":true})).unwrap();
    assert!(window.push(older).unwrap());
    let events = window.take();
    assert_eq!(events.len(), 99);
    assert_eq!(events[0].sequence, 351);
    assert_eq!(
        window.params("session", "main")["before"],
        301,
        "the raw boundary can precede the first visible event"
    );
    let mut earlier = Window::earlier(window.before.unwrap(), window.through.unwrap());
    let page: HistoryWindow = serde_json::from_value(json!({"events":(1..=300).map(event).collect::<Vec<_>>(),"through":449,"nextAfter":300,"nextBefore":1,"done":true})).unwrap();
    assert!(earlier.push(page).unwrap());
    assert_eq!(earlier.take().len(), 300);
    assert!(earlier.before.is_none());
    let page: DisplayPage = serde_json::from_value(json!({
        "items":[event(1),{"id":"compaction-2","sequence":2,"create_at":2,
            "type":"display","name":"compaction","data":{"strategy":"summary"}}],
        "through":2,"nextAfter":2,"nextBefore":1,"done":true
    }))
    .unwrap();
    let mut display = Window::default();
    assert_eq!(display.params("session", "main")["limit"], 64);
    assert!(display.push(page.into()).unwrap());
    let items = display.take();
    assert_eq!(items.len(), 2);
    assert_eq!(display.params("session", "main")["limit"], 256);
    assert!(
        matches!(&items[1].content, EventContent::CompactionSummary(summary) if summary.strategy=="summary")
    );
    assert_eq!(items[1].id, "compaction-2");
    let page: DisplayPage = serde_json::from_value(json!({
        "items":[{"id":"summary","sequence":50,"create_at":2,
            "type":"display","name":"compaction","data":{"strategy":"summary"}}],
        "through":50,"nextAfter":50,"nextBefore":50,"done":false
    }))
    .unwrap();
    let mut summary = Window::default();
    assert!(
        summary.push(page.into()).unwrap(),
        "a visible compaction must not trigger earlier history reads"
    );
    assert_eq!(summary.take().len(), 1);
}

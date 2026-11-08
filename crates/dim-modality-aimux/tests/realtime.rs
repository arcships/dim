//! The realtime relay against a local WebSocket stub: the URL and headers it
//! connects with, the frames it writes, the events it forwards, and the ends
//! it reports.
#![cfg(unix)]
use dim_agent_capabilities::host::{
    HttpModelConnection, ModalityConnection, ModelConnection, ModelNetwork, RealtimeDeclaration,
    ResolvedModelConnection,
};
use dim_agent_types::ModelProtocol;
use dim_modality::{
    ModalityAdapter, RealtimeEnd, RealtimeEvent, RealtimeFrame, RealtimeFrameKind,
    RealtimeTransport,
};
use dim_modality_aimux::MediaAdapter;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_util::sync::CancellationToken;

const KEY: &str = "sk-realtime-secret-key";
const MODEL: &str = "stepaudio-2.5-realtime";

/// A connection that points at `address`, as Provider resolves one for a
/// catalog entry that declares the openai-realtime transport.
fn connection(address: SocketAddr) -> ModalityConnection {
    let mut headers = HashMap::new();
    headers.insert("x-extra".to_owned(), "kept".to_owned());
    ModalityConnection {
        resolved: ResolvedModelConnection {
            connection: ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::OpenaiCompatible,
                base_url: format!("http://{address}/v1"),
                network: ModelNetwork {
                    proxy: None,
                    timeout_ms: None,
                },
                headers,
                bearer: Some(KEY.to_owned()),
                api_key: None,
                preset: None,
            }),
            failure_policy: None,
            recovery: None,
        },
        protocol: ModelProtocol::OpenaiCompatible,
        model: MODEL.to_owned(),
        realtime: Some(RealtimeDeclaration {
            protocol: "openai-realtime".into(),
            path: "/realtime".into(),
        }),
    }
}

/// The handshake callback the stub accepts with: tungstenite's `Callback`
/// hands back an `ErrorResponse` of its own size, which is not ours to box.
#[allow(clippy::result_large_err)]
fn checked(
    request: &Request,
    response: Response,
) -> Result<Response, http::Response<Option<String>>> {
    assert_eq!(
        request.uri().path_and_query().unwrap().as_str(),
        format!("/v1/realtime?model={MODEL}")
    );
    assert_eq!(
        request.headers().get("authorization").unwrap(),
        format!("Bearer {KEY}").as_str()
    );
    assert_eq!(request.headers().get("x-extra").unwrap(), "kept");
    Ok(response)
}

#[allow(clippy::result_large_err)]
fn accepted(
    _request: &Request,
    response: Response,
) -> Result<Response, http::Response<Option<String>>> {
    Ok(response)
}

#[allow(clippy::result_large_err)]
fn unauthorized(
    request: &Request,
    _response: Response,
) -> Result<Response, http::Response<Option<String>>> {
    assert_eq!(
        request.headers().get("authorization").unwrap(),
        format!("Bearer {KEY}").as_str()
    );
    Err(http::Response::builder()
        .status(401)
        .body(Some("provider-body".to_owned()))
        .unwrap())
}

/// The next event the relay reports, or a failure naming what arrived.
async fn next_event(events: &mut mpsc::Receiver<RealtimeEvent>) -> RealtimeEvent {
    tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
        .await
        .expect("the relay reported an event in time")
        .expect("the relay kept its event sender")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_relay_connects_with_the_declared_path_and_relays_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stub = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, checked)
            .await
            .unwrap();
        let text = socket.next().await.unwrap().unwrap();
        let Message::Text(text) = text else {
            panic!("the relay's first frame is text")
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "session.update");
        let binary = socket.next().await.unwrap().unwrap();
        let Message::Binary(bytes) = binary else {
            panic!("an audio frame goes on the wire binary")
        };
        assert_eq!(bytes.as_ref(), b"hi");
        socket
            .send(Message::Text(r#"{"type":"session.created"}"#.into()))
            .await
            .unwrap();
        // The provider ends the session.
        socket.close(None).await.unwrap();
        // The relay's own close frame.
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    });

    let adapter = MediaAdapter::new();
    let (events, mut received) = mpsc::channel(16);
    let opened = adapter
        .realtime_open(
            connection(address),
            json!({}),
            events,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        opened.channel_id.starts_with("rt_"),
        "{}",
        opened.channel_id
    );
    assert_eq!(opened.transport, RealtimeTransport::Relay);
    assert!(opened.connection.is_none());
    adapter
        .realtime_send(
            &opened.channel_id,
            RealtimeFrame {
                kind: RealtimeFrameKind::Text,
                data: json!({"type": "session.update", "session": {"instructions": "hi"}}),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    adapter
        .realtime_send(
            &opened.channel_id,
            RealtimeFrame {
                kind: RealtimeFrameKind::Audio,
                data: json!("aGk="),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    match next_event(&mut received).await {
        RealtimeEvent::Event(event) => {
            assert_eq!(event, json!({"type": "session.created"}));
        }
        _ => panic!("the provider's event is forwarded"),
    }
    match next_event(&mut received).await {
        RealtimeEvent::Ended(RealtimeEnd::Provider) => {}
        _ => panic!("the provider's close is reported as its end"),
    }
    adapter.realtime_close(&opened.channel_id).await.unwrap();
    stub.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_provider_message_over_the_bound_ends_the_session_at_the_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stub: JoinHandle<()> = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, accepted)
            .await
            .unwrap();
        // Over the relay's 1 MiB socket bound; the stub's own limit is
        // tungstenite's default, so the write is legal for it.
        let oversized = format!(
            r#"{{"type":"response.text.delta","padding":"{}"}}"#,
            "x".repeat(2 * 1024 * 1024)
        );
        let _ = socket.send(Message::Text(oversized.into())).await;
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    });

    let adapter = MediaAdapter::new();
    let (events, mut received) = mpsc::channel(16);
    let opened = adapter
        .realtime_open(
            connection(address),
            json!({}),
            events,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    match next_event(&mut received).await {
        RealtimeEvent::Ended(RealtimeEnd::Error(message)) => {
            assert!(!message.contains(KEY), "{message}");
        }
        _ => panic!("an oversized provider message ends the session with an error"),
    }
    adapter.realtime_close(&opened.channel_id).await.unwrap();
    stub.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_handshake_reports_the_status_and_no_secret() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stub = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = tokio_tungstenite::accept_hdr_async(stream, unauthorized).await;
    });

    let adapter = MediaAdapter::new();
    let (events, _received) = mpsc::channel(16);
    let error = adapter
        .realtime_open(
            connection(address),
            json!({}),
            events,
            CancellationToken::new(),
        )
        .await
        .expect_err("a rejected handshake fails the open");
    let message = error.to_string();
    assert!(message.contains("401"), "{message}");
    assert!(!message.contains(KEY), "{message}");
    assert!(!message.contains("provider-body"), "{message}");
    stub.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_relay_refuses_control_frames_and_unknown_channels() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stub = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, accepted)
            .await
            .unwrap();
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    });

    let adapter = MediaAdapter::new();
    let (events, _received) = mpsc::channel(16);
    let opened = adapter
        .realtime_open(
            connection(address),
            json!({}),
            events,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let refused = adapter
        .realtime_send(
            &opened.channel_id,
            RealtimeFrame {
                kind: RealtimeFrameKind::Control,
                data: json!({"type": "ice"}),
            },
            CancellationToken::new(),
        )
        .await;
    assert!(refused.is_err(), "a relay carries no control frames");
    let unknown = adapter
        .realtime_send(
            "rt_missing",
            RealtimeFrame {
                kind: RealtimeFrameKind::Text,
                data: json!({"type": "session.update"}),
            },
            CancellationToken::new(),
        )
        .await;
    assert!(unknown.is_err());
    adapter.realtime_close(&opened.channel_id).await.unwrap();
    stub.await.unwrap();
}

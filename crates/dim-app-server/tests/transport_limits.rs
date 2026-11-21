use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::{AgentBuilder, AgentConfig, Feature, Names, SessionIdentity};
use dim_app_server::{RunningServer, ServerConfig, serve};
use futures_util::{SinkExt, StreamExt};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpStream, UnixStream},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

const LIMIT: usize = 16 * 1024 * 1024;
enum Client {
    Local(BufReader<UnixStream>),
    Ws(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}
impl Client {
    async fn connect(server: &RunningServer, ws: bool) -> Self {
        let mut client = if ws {
            Self::Ws(Box::new(
                tokio_tungstenite::connect_async(format!(
                    "ws://{}/ws",
                    server.websocket_address.unwrap()
                ))
                .await
                .unwrap()
                .0,
            ))
        } else {
            Self::Local(BufReader::new(
                UnixStream::connect(server.unix_path.as_ref().unwrap())
                    .await
                    .unwrap(),
            ))
        };
        client.send(json!({"jsonrpc":"2.0","id":0,"method":"hello","params":{"protocolVersion":3,"clientKind":"other"}}).to_string()).await.unwrap();
        assert!(client.read().await.unwrap()["result"].is_object());
        client
    }
    async fn send(&mut self, text: String) -> Result<(), ()> {
        match self {
            Self::Local(s) => {
                s.get_mut()
                    .write_all(text.as_bytes())
                    .await
                    .map_err(|_| ())?;
                s.get_mut().write_all(b"\n").await.map_err(|_| ())
            }
            Self::Ws(s) => s.send(Message::Text(text.into())).await.map_err(|_| ()),
        }
    }
    async fn read(&mut self) -> Option<Value> {
        match self {
            Self::Local(s) => {
                let mut text = String::new();
                match s.read_line(&mut text).await {
                    Ok(n) if n > 0 && text.ends_with('\n') => {
                        Some(serde_json::from_str(&text).unwrap())
                    }
                    _ => None,
                }
            }
            Self::Ws(s) => loop {
                match s.next().await {
                    Some(Ok(Message::Text(t))) => break Some(serde_json::from_str(&t).unwrap()),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    _ => break None,
                }
            },
        }
    }
}
async fn launch() -> RunningServer {
    launch_with(ServerConfig::default().write_timeout, |_| {}).await
}
async fn launch_with(
    write_timeout: Duration,
    extra: impl FnOnce(&mut AgentBuilder),
) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    extra(&mut builder);
    serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-limits-{}/server.sock", uuid::Uuid::new_v4())),
            ),
            write_timeout,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
fn padded_ping(size: usize) -> String {
    let mut text = json!({"jsonrpc":"2.0","id":1,"method":"daemon.ping","params":{}}).to_string();
    text.extend(std::iter::repeat_n(' ', size - text.len()));
    text
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boundary_frames_and_oversized_frames_on_both_transports() {
    for ws in [false, true] {
        let server = launch().await;
        let mut client = Client::connect(&server, ws).await;
        client.send(padded_ping(LIMIT)).await.unwrap();
        assert!(client.read().await.unwrap()["result"].is_object());
        if !ws {
            let mut text = padded_ping(LIMIT);
            text.push('\r');
            client.send(text).await.unwrap();
            assert!(client.read().await.unwrap()["result"].is_object());
        }
        let _ = client.send(padded_ping(LIMIT + 1)).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(5), client.read())
                .await
                .unwrap()
                .is_none(),
            "oversized frame must close, not execute"
        );
        server.shutdown();
        tokio::time::timeout(Duration::from_secs(5), server.join())
            .await
            .unwrap()
            .unwrap();
    }
}

struct HoldCleanup {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    identity: Key<SessionIdentity>,
}
impl HoldCleanup {
    fn new(
        names: &Names<'_>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            entered,
            release,
            identity: names.key()?,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_ndjson_and_fragmented_websocket_are_bounded() {
    use tokio_tungstenite::tungstenite::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    let server = launch().await;
    let mut local = Client::connect(&server, false).await;
    if let Client::Local(s) = &mut local {
        let _ = s
            .get_mut()
            .write_all(padded_ping(LIMIT + 1).as_bytes())
            .await;
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(5), local.read())
            .await
            .unwrap()
            .is_none()
    );
    let mut ws = Client::connect(&server, true).await;
    let send_fragments = async |client: &mut Client, text: String| {
        let Client::Ws(s) = client else {
            unreachable!()
        };
        let split = text.len() / 2;
        s.send(Message::Frame(Frame::message(
            text.as_bytes()[..split].to_vec(),
            OpCode::Data(Data::Text),
            false,
        )))
        .await
        .unwrap();
        let _ = s
            .send(Message::Frame(Frame::message(
                text.as_bytes()[split..].to_vec(),
                OpCode::Data(Data::Continue),
                true,
            )))
            .await;
    };
    send_fragments(&mut ws, padded_ping(LIMIT)).await;
    assert!(ws.read().await.unwrap()["result"].is_object());
    send_fragments(&mut ws, padded_ping(LIMIT + 1)).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), ws.read())
            .await
            .unwrap()
            .is_none()
    );
    server.shutdown();
    tokio::time::timeout(Duration::from_secs(5), server.join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_size_limit_preserves_large_valid_replies() {
    for ws in [false, true] {
        let server = launch().await;
        let mut client = Client::connect(&server, ws).await;
        let ping = |id: String| json!({"jsonrpc":"2.0","id":id,"method":"daemon.ping"}).to_string();
        client
            .send(ping("x".repeat(15 * 1024 * 1024)))
            .await
            .unwrap();
        assert_eq!(
            client.read().await.unwrap()["id"].as_str().unwrap().len(),
            15 * 1024 * 1024
        );
        let overhead = ping(String::new()).len();
        let text = ping("x".repeat(LIMIT - overhead));
        assert_eq!(text.len(), LIMIT);
        client.send(text).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), client.read())
                .await
                .unwrap()
                .is_none()
        );
        server.shutdown();
        tokio::time::timeout(Duration::from_secs(5), server.join())
            .await
            .unwrap()
            .unwrap();
    }
}
impl DynPlugin for HoldCleanup {
    fn name(&self) -> &str {
        "test.transport-hold-cleanup"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            let _identity = ctx.require_at(&self.identity)?;
            ctx.effect(move || async move {
                entered.notify_one();
                release.notified().await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_reserves_cleanup_and_disconnect_settles_accepted_commands() {
    for ws in [false, true] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let start = entered.clone();
        let finish = release.clone();
        let server = launch_with(ServerConfig::default().write_timeout, move |builder| {
            builder.require_feature(Feature::new("test.transport-hold-cleanup").session(
                "test.transport-hold-cleanup",
                move |names| {
                    Ok(Box::new(HoldCleanup::new(
                        names,
                        start.clone(),
                        finish.clone(),
                    )?))
                },
            ));
        })
        .await;
        let mut client = Client::connect(&server, ws).await;
        client
            .send(
                json!({"jsonrpc":"2.0","id":1,"method":"session.create","params":{"cwd":"/tmp"}})
                    .to_string(),
            )
            .await
            .unwrap();
        let created = client.read().await.unwrap();
        let sid = created["result"]["sessionId"].as_str().unwrap();
        for id in 2..66 {
            client.send(json!({"jsonrpc":"2.0","id":id,"method":"session.close","params":{"sessionId":sid}}).to_string()).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        client
            .send(json!({"jsonrpc":"2.0","id":100,"method":"daemon.ping"}).to_string())
            .await
            .unwrap();
        let busy = tokio::time::timeout(Duration::from_secs(5), client.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(busy["id"], 100);
        assert_eq!(busy["error"]["data"]["appError"], "connection_busy");
        for id in 66..75 {
            client.send(json!({"jsonrpc":"2.0","id":id,"method":"session.close","params":{"sessionId":sid}}).to_string()).await.unwrap();
        }
        let busy = tokio::time::timeout(Duration::from_secs(5), client.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(busy["id"], 74);
        assert_eq!(busy["error"]["data"]["appError"], "connection_busy");
        drop(client);
        server.shutdown();
        let mut join = Box::pin(server.join());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut join)
                .await
                .is_err(),
            "accepted cleanup was abandoned on disconnect"
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), join)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_consumer_closes_and_server_settles_on_both_transports() {
    for ws in [false, true] {
        let server = launch_with(Duration::from_secs(2), |_| {}).await;
        let mut client = Client::connect(&server, ws).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            for i in 0..100 {
                let id = format!("{i}{}", "x".repeat(1024 * 1024));
                if client
                    .send(
                        json!({"jsonrpc":"2.0","id":id,"method":"daemon.ping","params":{}})
                            .to_string(),
                    )
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut replies = 0;
            while client.read().await.is_some() {
                replies += 1;
            }
            assert!(
                replies < 100,
                "slow consumer unexpectedly accepted every reply"
            );
        })
        .await
        .expect("slow consumer was not closed");
        server.shutdown();
        tokio::time::timeout(Duration::from_secs(5), server.join())
            .await
            .unwrap()
            .unwrap();
    }
}

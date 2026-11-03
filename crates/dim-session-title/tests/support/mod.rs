// Each test binary uses the helpers it needs; the rest are shared.
#![allow(dead_code)]
use axum::{Json, body::Body, extract::State};
use dim_app_server::{RunningServer, ServerConfig};
use dim_client::{Client, ConnectionConfig, Server};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::sync::{mpsc, oneshot};

pub struct Store(pub PathBuf);
impl Store {
    pub fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let path = PathBuf::from("/tmp").join(format!("dim-title-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    pub fn config(&self) -> ServerConfig {
        ServerConfig {
            database_path: Some(self.0.join("dimcode.sqlite")),
            ..Default::default()
        }
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub async fn client(server: &RunningServer) -> Client {
    Client::connect(&ConnectionConfig {
        endpoint: server.unix_path.as_ref().unwrap().clone(),
        server: Server {
            program: "dimcode".into(),
            embedded: true,
        },
        profile: None,
        client_kind: "other",
        client_name: "SessionTitle flow",
    })
    .await
    .unwrap()
}

pub fn waiting(error: &dim_client::Error) -> bool {
    matches!(
        dim_client::app_error(error),
        Some("unknown_method" | "service_reloading")
    )
}

pub async fn when_served(client: &mut Client, method: &str, params: Value) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.rpc(method, params.clone()).await {
                Ok(value) => return value,
                Err(error) if waiting(&error) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => panic!("{method}: {error}"),
            }
        }
    })
    .await
    .expect("the Session method becomes available")
}

pub type Request = (Value, oneshot::Sender<String>);

pub async fn complete(
    State(sent): State<mpsc::UnboundedSender<Request>>,
    Json(request): Json<Value>,
) -> axum::response::Response {
    let (release, reply) = oneshot::channel();
    sent.send((request, release)).unwrap();
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(futures_util::stream::once(async move {
            let text = reply.await?;
            Ok::<_, oneshot::error::RecvError>(format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"id":"title", "choices":[{"index":0,
                    "delta":{"content":text}, "finish_reason":"stop"}]})
            ))
        })))
        .unwrap()
}

pub async fn configure(client: &mut Client, document: Value) {
    let read = client
        .rpc(
            "settings.read",
            json!({"source":"session-title","scope":"user"}),
        )
        .await
        .unwrap();

    client
        .rpc(
            "settings.update",
            json!({"source":"session-title","scope":"user",
        "expectedRevision":read["revision"],"document":document}),
        )
        .await
        .unwrap();
}

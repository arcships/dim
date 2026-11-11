//! The title a Session's creation requests (#1421): `sessions.launch` carries
//! `title`, Agents stores it in the Session's fixed configuration, and
//! SessionTitle writes it once as the first manual title. SessionTitle stays
//! the title's one writer (SessionTitle.md §2, session-title-metadata.md).
#![cfg(unix)]
mod support;

use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::{AgentBuilder, AgentConfig, Feature};
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::{Client, Message};
use rutis::host::CtxHost;
use rutis_bridge::session::HostDispatch;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use support::*;

const PROVIDER: &str = "018ec73f-0000-7000-8000-00000000000c";

/// `sessions` as a Session part injects it by name: the process row that
/// dispatches it is not a test's to add, so the part is.
#[derive(Clone, Default)]
struct Sessions(Arc<OnceLock<Arc<dyn HostDispatch>>>);
struct SessionsConsumer {
    sessions: Sessions,
    service: rutis::host::Key<dyn HostDispatch>,
}
impl SessionsConsumer {
    fn new(names: &dim_agent_runtime::Names<'_>, sessions: Sessions) -> rutis::Result<Self> {
        Ok(Self {
            sessions,
            service: names.named("sessions")?,
        })
    }
}
impl rutis::host::DynPlugin for SessionsConsumer {
    fn name(&self) -> &str {
        "sessions-consumer"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.service.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let service = ctx.require_at(&self.service)?;
            let _ = self.sessions.0.set(service);
            Ok(rutis::host::Effect::Done)
        })
    }
}
impl Sessions {
    async fn launch(&self, request: Value) -> Value {
        let service = self.0.get().expect("a Session injected sessions");
        match service.invoke("launch", json!([request]).into()).unwrap() {
            rutis_bridge::session::Value::Reference(reply) if reply.is_future() => {
                reply.wait_async().await.unwrap().json().unwrap()
            }
            reply => panic!("sessions replies asynchronously: {:?}", reply.json()),
        }
    }
}

fn builder(sessions: &Sessions) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_session_title::install(&mut builder);
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    let slot = sessions.clone();
    builder.require_feature(
        Feature::new("sessions-consumer").session("sessions-consumer", move |names| {
            Ok(Box::new(SessionsConsumer::new(names, slot.clone())?))
        }),
    );
    builder
}

/// A served product profile with SessionTitle and the `sessions` consumer.
/// The caller keeps the server alive; `start` returns it with the client.
async fn start(store: &Store) -> (RunningServer, Client, Sessions) {
    let sessions = Sessions::default();
    let server = serve(
        builder(&sessions),
        ServerConfig {
            product_home: Some(store.0.clone()),
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-initial-title-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            ..store.config()
        },
    )
    .await
    .unwrap();
    let mut client = client(&server).await;
    // The product profile resolves a launch's model through the configured
    // provider. No call is made: the initial title needs no model, so the
    // endpoint is never reached. A restarted daemon keeps the stored provider.
    if client
        .rpc("provider.list", json!({}))
        .await
        .is_ok_and(|listed| !listed.to_string().contains(PROVIDER))
    {
        client.rpc("provider.create",json!({"providerId":PROVIDER,"displayName":"Initial title fixture",
            "connection":{"driverKind":"openai-compatible","baseUrl":"http://127.0.0.1:1/v1","auth":{"type":"anonymous"}},
            "models":[{"modelId":"main","displayName":"Main","status":"active","capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000,16_384)}],"enabled":true})).await.unwrap();
    }
    // A restarted daemon keeps the stored selection too; only a fresh store
    // needs it made.
    let _ = client
        .rpc(
            "provider.select",
            json!({"providerId":PROVIDER,"modelId":"main"}),
        )
        .await;
    // The consumer needs one Session of its own to inject `sessions`; this
    // one is never launched into.
    client
        .rpc(
            "session.create",
            json!({"cwd":store.0,"tools":["wait"],"interactive":false,
                "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        )
        .await
        .unwrap();
    (server, client, sessions)
}

/// The title entry of `id`, once its notice has arrived. The caller attaches
/// first: a notice reaches only a client the Session is attached to.
async fn titled(client: &mut Client, id: &str, text: &str, source: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let listed = client.rpc("session.summaries", json!({})).await.unwrap();
            let Some(entry) = listed
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["sessionId"] == id)
                .cloned()
            else {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            };
            if entry["title"] == text
                && entry["titleSource"] == source
                && client.notices.iter().any(|notice| {
                    matches!(notice,
                    Message::Notice { method, params }
                    if method == "session.notice" && params["sessionId"] == id
                        && params["kind"] == "title"
                        && params["data"]["titleRevision"] == entry["titleRevision"])
                })
            {
                return entry;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the committed title and its notice arrive")
}

/// A Session whose creation requests a title gets it once, as the first manual
/// title, and a rename before a restart is never undone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_requested_title_is_written_once_and_a_later_rename_wins() {
    let store = Store::new();
    let (server, mut client, sessions) = start(&store).await;
    let launched = sessions
        .launch(json!({"cwd":store.0,"origin":"automation","title":"Nightly build"}))
        .await;
    assert_eq!(launched["ok"], true, "{launched}");
    let id = launched["value"]["sessionId"].as_str().unwrap().to_owned();
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    let entry = titled(&mut client, &id, "Nightly build", "manual").await;
    assert_eq!(entry["titleRevision"], 1, "{entry}");

    let renamed = when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":id,"title":"  Renamed by hand  "}),
    )
    .await;
    assert_eq!(renamed["title"], "Renamed by hand");
    titled(&mut client, &id, "Renamed by hand", "manual").await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // A daemon that loads the Session again sees a revision it must not
    // replace, so the rename survives.
    let (server, mut client, _) = start(&store).await;
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    let listed = client.rpc("session.summaries", json!({})).await.unwrap();
    let entry = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionId"] == id)
        .unwrap();
    assert_eq!(entry["title"], "Renamed by hand", "{entry}");
    assert_eq!(entry["titleSource"], "manual", "{entry}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A requested title longer than the operator's `maxLength` is bounded the way
/// a fallback title is; the text is not otherwise rewritten.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_requested_title_is_bounded_by_max_length() {
    let store = Store::new();
    let (server, mut client, sessions) = start(&store).await;
    configure(&mut client, json!({"enabled":true,"maxLength":12})).await;
    let launched = sessions
        .launch(json!({"cwd":store.0,"title":"A name much longer than the bound."}))
        .await;
    assert_eq!(launched["ok"], true, "{launched}");
    let id = launched["value"]["sessionId"].as_str().unwrap().to_owned();
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    titled(&mut client, &id, "A name much", "manual").await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A requested title with a control character writes nothing: rename's text
/// rule refuses it, so the Session stays untitled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_requested_title_with_a_control_character_writes_nothing() {
    let store = Store::new();
    let (server, mut client, sessions) = start(&store).await;
    let launched = sessions
        .launch(json!({"cwd":store.0,"title":"Nightly\u{7}build"}))
        .await;
    assert_eq!(launched["ok"], true, "{launched}");
    let id = launched["value"]["sessionId"].as_str().unwrap().to_owned();
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    // The part's write is what could title it, and it runs before the part
    // serves rename: a part still loading answers `service_reloading`, and a
    // served one answers the invalid title this call sends.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client
                .rpc("session.rename", json!({"sessionId":id,"title":0}))
                .await;
            if reply.is_err_and(|error| dim_client::app_error(&error) == Some("invalid_params")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("SessionTitle serves rename");
    let listed = client.rpc("session.summaries", json!({})).await.unwrap();
    let entry = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionId"] == id)
        .unwrap();
    assert_eq!(entry["title"], Value::Null, "{entry}");
    assert_eq!(entry["titleRevision"], 0, "{entry}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

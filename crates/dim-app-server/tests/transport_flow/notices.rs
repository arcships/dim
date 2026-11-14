//! Session broadcast notices from their owner to attached clients (C6,
//! design/appserver/session-notices.md §9).
use super::*;
use dim_agent_capabilities::session_broadcast::{
    SessionBroadcast, SessionBroadcastEvent, SessionNotice, notify, session_channel,
};
use dim_agent_runtime::SessionIdentity;
use rutis::Ctx;
use rutis::host::CtxHost;

/// A Session-scoped stand-in for an owner of process-level state. Like
/// SessionTitle's flow, it reacts to `input/applied` observed through the
/// broadcast and announces its own committed change with `notify`.
struct Owner {
    ctx: Ctx,
    session_id: String,
}
impl Owner {
    fn on_record(&self, record: &SessionBroadcastEvent) {
        if let SessionBroadcastEvent::Event { branch_id, event } = record
            && branch_id == "main"
            && matches!(&event.kind, EventKind::InputApplied(data) if data.mode.unwrap_or(dim_agent_types::InputMode::Queue) == dim_agent_types::InputMode::Queue)
        {
            notify(
                &self.ctx,
                &self.session_id,
                SessionNotice {
                    source: "test.owner".into(),
                    kind: "applied".into(),
                    data: json!({"sequence": event.sequence, "nested": [1, "two", null]}),
                },
            );
        }
    }
}
struct OwnerPlugin {
    identity: rutis::host::Key<SessionIdentity>,
}
impl OwnerPlugin {
    fn new(names: &dim_agent_runtime::Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            identity: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for OwnerPlugin {
    fn name(&self) -> &str {
        "test.notice-owner"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let owner = Owner {
                ctx: ctx.clone(),
                session_id: identity.session_id.clone(),
            };
            ctx.listen::<SessionBroadcast>()
                .name(session_channel(&identity.session_id))
                .on(move |event, _| {
                    owner.on_record(&event.record);
                    async { Ok(None) }
                })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// Every message the client received before an idle reply. The reply waits
/// for this subscription's delivery fence, which follows the turn's events and
/// any notice emitted while they were delivered.
async fn delivered(client: &mut Client, sid: &str) -> Vec<Value> {
    let idle = client
        .rpc(
            "session.idle",
            json!({"sessionId": sid, "branchId": "main"}),
        )
        .await;
    assert_ok(&idle);
    let seen = std::mem::take(&mut client.notices);
    assert!(
        seen.iter().any(|value| value["method"] == "session.event"
            && value["params"]["event"]["type"] == "turn"
            && value["params"]["event"]["name"] == "end"),
        "{seen:?}"
    );
    seen
}

/// Creates a Session, attaches every client and submits one input through
/// the first.
async fn attached_submit(clients: &mut [&mut Client]) -> String {
    let created = clients[0].rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    for client in clients.iter_mut() {
        let attached = client
            .rpc("session.attach", json!({"sessionId": sid}))
            .await;
        assert_ok(&attached);
    }
    let input = json!({"sessionId": sid, "branchId": "main", "text": "hello"});
    assert_ok(&clients[0].rpc("session.submit", input).await);
    sid
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_notice_follows_its_observed_event_to_v3_clients_only() {
    for _ in 0..5 {
        let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), |builder| {
            builder.require_feature(
                Feature::new("test.notice-owner").session("test.notice-owner", |names| {
                    Ok(Box::new(OwnerPlugin::new(names)?))
                }),
            );
        })
        .await;
        let mut ipc = Client::new_version(&server, false, 3).await;
        let mut ws = Client::new_version(&server, true, 3).await;
        let mut legacy = Client::new_version(&server, false, 2).await;
        let sid = attached_submit(&mut [&mut ipc, &mut ws]).await;
        let legacy_sid = attached_submit(&mut [&mut legacy]).await;
        for client in [&mut ipc, &mut ws] {
            let seen = delivered(client, &sid).await;
            let applied = seen
                .iter()
                .position(|value| {
                    value["method"] == "session.event"
                        && value["params"]["event"]["type"] == "input"
                        && value["params"]["event"]["name"] == "applied"
                        && (value["params"]["event"]["data"]["mode"].is_null()
                            || value["params"]["event"]["data"]["mode"] == "queue")
                })
                .expect("input/applied reaches the client");
            let notices: Vec<_> = seen
                .iter()
                .enumerate()
                .filter(|(_, value)| value["method"] == "session.notice")
                .collect();
            assert_eq!(notices.len(), 1, "{seen:?}");
            let (position, notice) = notices[0];
            assert!(applied < position, "the notice follows its event: {seen:?}");
            assert_eq!(
                notice["params"],
                json!({
                    "sessionId": sid,
                    "source": "test.owner",
                    "kind": "applied",
                    "data": {
                        "sequence": seen[applied]["params"]["event"]["sequence"],
                        "nested": [1, "two", null],
                    },
                })
            );
        }
        let seen = delivered(&mut legacy, &legacy_sid).await;
        assert!(seen.iter().any(|value| value["method"] == "session.event"
            && value["params"]["event"]["name"] == "applied"));
        assert!(
            seen.iter().all(|value| value["method"] != "session.notice"),
            "{seen:?}"
        );
        server.shutdown();
        server.join().await.unwrap();
    }
}

fn notice(data: &str) -> SessionNotice {
    SessionNotice {
        source: "test.process-owner".into(),
        kind: "changed".into(),
        data: json!(data),
    }
}

/// The client's next notice, which must be `data`: any notice emitted
/// earlier on the channel would have arrived first.
async fn next_notice(client: &mut Client, sid: &str, data: &str) {
    assert_eq!(
        client.notice("session.notice").await["params"],
        json!({"sessionId": sid, "source": "test.process-owner", "kind": "changed", "data": data})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notices_without_a_listener_are_dropped_and_never_replayed() {
    let directory = std::env::temp_dir().join(format!("dim-c6-notices-{}", uuid::Uuid::new_v4()));
    let (server, agent) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            database_path: Some(directory.join("dimcode.sqlite")),
            ..Default::default()
        },
    )
    .await;
    for _ in 0..3 {
        for transport in [false, true] {
            let mut client = Client::new_version(&server, transport, 3).await;
            let created = client.rpc("session.create", create(json!([]))).await;
            let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
            let attach = json!({"sessionId": sid});
            assert_ok(&client.rpc("session.attach", attach.clone()).await);
            server.notify(&sid, notice("attached"));
            next_notice(&mut client, &sid, "attached").await;

            assert_ok(&client.rpc("session.detach", attach.clone()).await);
            server.notify(&sid, notice("detached"));
            assert_ok(&client.rpc("session.attach", attach.clone()).await);
            server.notify(&sid, notice("reattached"));
            next_notice(&mut client, &sid, "reattached").await;

            assert_ok(&client.rpc("session.detach", attach.clone()).await);
            super::collaboration::unload(&agent, &sid).await;
            server.notify(&sid, notice("unloaded"));
            assert_ok(&client.rpc("session.attach", attach.clone()).await);
            server.notify(&sid, notice("restored"));
            next_notice(&mut client, &sid, "restored").await;

            assert_ok(&client.rpc("session.close", attach).await);
            server.notify(&sid, notice("closed"));
            let closed = client.notice("session.closed").await;
            assert_eq!(closed["params"]["sessionId"], sid.as_str());
        }
    }
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

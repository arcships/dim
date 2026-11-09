use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use dim_app_server::link::{LinkConfig, LinkMode, LinkPlugin};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect};
use rutis_bridge::{
    LocalId, Session,
    channel::PeerId,
    session::{HostDispatch, Reply, Value, host_key},
    websocket::{self, Admitted, Limits, ListenerConfig, Upgrade},
};
use serde_json::json;

struct Clock(AtomicU64);

impl HostDispatch for Clock {
    fn invoke(&self, _: &str, _: Value) -> Reply {
        Ok(json!(self.0.fetch_add(1, Ordering::SeqCst)).into())
    }

    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({ "now": "sync" }))
    }
}

struct ClockPlugin;

impl DynPlugin for ClockPlugin {
    fn name(&self) -> &str {
        "clock"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.provide_at(
                host_key("clock"),
                Arc::new(Clock(AtomicU64::new(0))) as Arc<dyn HostDispatch>,
            )?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_mounts_a_remote_kernel_and_withdraws_its_services() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();
    let listener_root = Ctx::root().unwrap();
    listener_root
        .provide(LocalId(PeerId::new("listener").unwrap()))
        .unwrap();
    listener_root.plugin(ClockPlugin);

    let (sender, mut accepted) = tokio::sync::mpsc::unbounded_channel();
    let protocol = format!("rutis.{}", rutis_bridge::session::ENDPOINT_PROTOCOL);
    let admit: websocket::Admit = Arc::new(move |_: Upgrade<'_>| {
        let sender = sender.clone();
        Ok(Admitted {
            protocol: protocol.clone(),
            deliver: Box::new(move |channel| {
                let _ = sender.send(channel);
            }),
        })
    });
    let config = ListenerConfig::new("test", "127.0.0.1:0".parse().unwrap());
    let listener = websocket::listen(&config, &Limits::default(), admit).unwrap();
    let address = listener.local_addr();
    let relay = {
        let root = listener_root.clone();
        tokio::spawn(async move {
            let mut sessions = Vec::new();
            while let Some(channel) = accepted.recv().await {
                sessions.push(root.plugin(Session::new(channel)));
            }
        })
    };

    let dialer = Ctx::root().unwrap();
    dialer
        .provide(LocalId(PeerId::new("dialer").unwrap()))
        .unwrap();
    let link = dialer.plugin(LinkPlugin::new(LinkConfig {
        mode: LinkMode::Dial,
        address: format!("ws://{address}/rutis"),
        token: Some("test".into()),
        ca_pem: None,
        cert_pem: None,
        key_pem: None,
    }));

    let mut clock = None;
    for _ in 0..100 {
        if let Some(found) = dialer.get_at::<dyn HostDispatch>(&host_key("clock")) {
            clock = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let clock = clock.expect("the far end's service is visible");
    let reply = tokio::task::spawn_blocking(move || clock.invoke("now", Value::List(vec![])))
        .await
        .unwrap()
        .expect("the clock answers");
    assert_eq!(Value::json(reply).unwrap(), json!(0));

    link.dispose().await.unwrap();
    for _ in 0..100 {
        if dialer
            .get_at::<dyn HostDispatch>(&host_key("clock"))
            .is_none()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        dialer
            .get_at::<dyn HostDispatch>(&host_key("clock"))
            .is_none(),
        "the service is withdrawn when the session ends"
    );

    relay.abort();
    drop(listener);
}

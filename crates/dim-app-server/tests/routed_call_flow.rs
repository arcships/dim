#![cfg(unix)]
//! A routed call by id against a minimal host that has only the offline model
//! catalog: Agents admits the Session, so the load-and-hold path of ADR 027 D5
//! is the production one (#2127).
//!
//! Unix only: the host listens on a Unix socket. `scripts/test-windows-native.mjs`
//! compiles `dim-cli --lib` and the two flow targets it names, so Windows never
//! builds this file.

use dim_agent_capabilities::defaults;
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{RunningServer, ServerConfig, serve};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, TypeKey};
use rutis_bridge::session::{Connection, Dispatch, Endpoint, HostDispatch, Reply, Value, host_key};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// A process row that hands its context to the test, so the test speaks to the
/// assembly as a plugin of another language would.
struct Probe(Arc<OnceLock<Ctx>>);

impl DynPlugin for Probe {
    fn name(&self) -> &str {
        "routed-call-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![host_key("sessions").erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            self.0.set(ctx.clone()).ok();
            Ok(Effect::Done)
        })
    }
}

struct Faces(Ctx);

impl Dispatch for Faces {
    fn invoke(&self, _: &Connection, target: &str, method: &str, args: Value) -> Reply {
        self.0
            .require_at::<dyn HostDispatch>(&host_key(target))
            .unwrap()
            .invoke(method, args)
    }
}

struct Rig {
    home: tempfile::TempDir,
    server: RunningServer,
    _host: Connection,
    client: Connection,
}

impl Rig {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            home.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let catalog = dim_agent_aimux::config::ModelConfigFile::load(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../dim-agent-aimux/examples/offline.json"),
        )
        .expect("the offline model catalog loads");
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        let observed = Arc::new(OnceLock::new());
        let probe = observed.clone();
        builder.plugin(move |_| Ok(Box::new(Probe(probe.clone())) as Box<dyn DynPlugin>));
        let server = serve(
            builder,
            ServerConfig {
                startup_timeout: Duration::from_secs(60),
                database_path: Some(home.path().join("dimcode.sqlite")),
                unix_path: Some(home.path().join("native.sock")),
                model_catalog: Some(Arc::new(
                    dim_agent_aimux::AimuxModels::from_config(catalog).expect("the catalog builds"),
                )),
                ..Default::default()
            },
        )
        .await
        .expect("the minimal host serves");
        let ctx = observed.get().expect("the probe loads").clone();
        let (a, b) = rutis_bridge::memory::pair();
        let peer = |id: &str| Endpoint::rust(rutis_bridge::channel::PeerId::new(id).unwrap());
        let host = Connection::open(a, Arc::new(Faces(ctx.clone())), peer("routed-host")).unwrap();
        let client = Connection::open(b, Arc::new(Faces(ctx)), peer("routed-client")).unwrap();
        client.ready().await.unwrap();
        Self {
            home,
            server,
            _host: host,
            client,
        }
    }

    async fn call(&self, method: &str, request: serde_json::Value) -> serde_json::Value {
        tokio::time::timeout(
            Duration::from_secs(30),
            self.client
                .invoke_settled("sessions", method, Value::List(vec![request.into()]))
                .unwrap(),
        )
        .await
        .expect("the call answers")
        .expect("the call is served")
        .json()
        .expect("a reply is data")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_routed_call_loads_and_holds_an_admitted_session() {
    let rig = Rig::start().await;
    let launched = rig
        .call("launch", json!({"cwd": rig.home.path().to_string_lossy()}))
        .await;
    let id = launched["value"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a Session launches through Agents: {launched}"))
        .to_owned();
    let before = rig
        .call(
            "call",
            json!({"sessionId": id, "service": "sessionConfig", "method": "get"}),
        )
        .await;
    assert_eq!(before["ok"], true, "an admitted Session answers: {before}");
    let session = rig
        .server
        .agent()
        .expect("the profile has an Agent")
        .get_session(&id)
        .expect("the launched Session is loaded");
    session.idle("main").await.unwrap();
    assert!(
        session.unload_if_idle().await.unwrap(),
        "an idle admitted Session unloads"
    );
    assert!(
        rig.server.agent().unwrap().get_session(&id).is_none(),
        "the idle release takes the Session out of the Agent"
    );
    let restored = rig
        .call(
            "call",
            json!({"sessionId": id, "service": "sessionConfig", "method": "get"}),
        )
        .await;
    assert_eq!(
        restored["ok"], true,
        "sessions.call reloads an admitted Session (D5): {restored}"
    );
    rig.server.shutdown();
    rig.server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_during_an_agents_restart_is_retryable() {
    let rig = Rig::start().await;
    let launched = rig
        .call("launch", json!({"cwd": rig.home.path().to_string_lossy()}))
        .await;
    let id = launched["value"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a Session launches through Agents: {launched}"))
        .to_owned();
    let call = json!({"sessionId": id, "service": "sessionConfig", "method": "get"});
    let calls = async {
        let mut transient = 0usize;
        let mut answered = 0usize;
        for _ in 0..150 {
            let reply = rig.call("call", call.clone()).await;
            if reply["error"]["code"] == "session_reloading" {
                transient += 1;
                assert_eq!(reply["error"]["retryable"], true, "{reply}");
            } else if reply["ok"] == true {
                answered += 1;
            }
        }
        (transient, answered)
    };
    let ((transient, answered), restarted) = tokio::join!(calls, async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        rig.server.restart("Agents").await
    });
    restarted.expect("Agents restarts");
    println!("transient={transient} answered={answered}");
    assert!(
        transient > 0,
        "a call during the restart answers a retryable session_reloading: transient={transient} answered={answered}"
    );
    let retried = rig.call("call", call.clone()).await;
    assert_eq!(
        retried["ok"], true,
        "the retry after the restart answers: {retried}"
    );
    println!("transient={transient} answered={answered}");
    rig.server.shutdown();
    rig.server.join().await.unwrap();
}

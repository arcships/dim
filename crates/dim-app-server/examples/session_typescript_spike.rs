//! C4 transport probe. This example installs no product bridge or protocol.
use dim_agent_capabilities::{defaults, session_broadcast::*};
use dim_agent_runtime::{
    AGENT_GROUP, AgentBuilder, AgentConfig, Feature, Modules, Names, SESSION_GROUP, SessionConfig,
    SessionIdentity, register_part,
};
use dim_agent_types::{InputMode, ToolCall, ToolDefinition, ToolResult};
use futures_util::FutureExt;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, InstanceId, Key, TypeKey};
use rutis_bridge::{
    LocalId, PeerId, Session,
    pipe::Processes,
    runtime::{Launcher, Stdio},
    session::{HostDispatch, Reply, Value, arg, host_key_in, settle},
};
use rutis_loader::runtime::{SessionResolver, SessionRows};
use rutis_loader::{Chain, Layer, LoaderOptions, LoaderPlugin};
use serde_json::{Value as Json, json};
use std::{
    panic::AssertUnwindSafe,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The Node runtime's connection row: its process, greeted, as a session.
struct Node {
    launcher: Launcher,
    anchor: PathBuf,
}
impl DynPlugin for Node {
    fn name(&self) -> &str {
        "node"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let failed = |error: String| rutis::Error::from(ErrorKind::PluginFailed(error.into()));
            let processes = Processes::default();
            let ended = processes.clone();
            ctx.effect(move || async move {
                ended.kill();
                ended.ended().await;
                Ok(())
            })?;
            let id = PeerId::new("node").map_err(|error| failed(error.to_string()))?;
            let channel = self
                .launcher
                .start(&id, &self.anchor, &processes)
                .await
                .map_err(|error| failed(error.to_string()))?;
            let channel = rutis_bridge::pipe::greeted(channel, Duration::from_secs(30))
                .await
                .map_err(failed)?;
            ctx.plugin(Session::new(channel));
            Ok(Effect::Done)
        })
    }
}

/// Sends its context once the Node rows are up.
struct Anchor {
    contexts: mpsc::UnboundedSender<Ctx>,
    rows: Key<SessionRows>,
}
impl Anchor {
    fn new(_: &Names<'_>, contexts: mpsc::UnboundedSender<Ctx>) -> rutis::Result<Self> {
        Ok(Self {
            contexts,
            rows: SessionRows::key("node"),
        })
    }
}
impl DynPlugin for Anchor {
    fn name(&self) -> &str {
        "C4Anchor"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.rows.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            self.contexts
                .send(ctx.clone())
                .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
            Ok(Effect::Done)
        })
    }
}

struct Loaded {
    session_id: String,
    session: InstanceId,
    ctx: Ctx,
    closed: Arc<AtomicUsize>,
}
/// The probe of one Session, built for its instance.
struct SessionProbe(
    InstanceId,
    mpsc::UnboundedSender<Loaded>,
    rutis::host::Key<SessionIdentity>,
);
struct SessionHost(String, Arc<AtomicUsize>);
impl HostDispatch for SessionHost {
    fn invoke(&self, method: &str, _: Value) -> Reply {
        match method {
            "identity" => Ok(json!(self.0).into()),
            "closed" => {
                self.1.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Undefined)
            }
            _ => Err(rutis_bridge::session::Error::value(format!(
                "unknown method {method}"
            ))),
        }
    }
    fn methods(&self) -> Option<Json> {
        Some(json!({"identity":"sync","closed":"sync"}))
    }
}
struct BroadcastProbe(InstanceId);
impl BroadcastProbe {
    async fn on_record(
        &self,
        ctx: &Ctx,
        record: &SessionBroadcastEvent,
    ) -> rutis::Result<Option<()>> {
        let record = match record {
            SessionBroadcastEvent::Event { branch_id, event } => {
                json!({"kind":"event","branchId":branch_id,"event":event})
            }
            SessionBroadcastEvent::Fence { recipient, token } if recipient == "c4-spike" => {
                json!({"kind":"fence","token":token})
            }
            SessionBroadcastEvent::Notice(_) | SessionBroadcastEvent::Fence { .. } => {
                return Ok(None);
            }
            SessionBroadcastEvent::Closed => json!({"kind":"closed"}),
        };
        if let Some(target) = ctx.get_at(&host_key_in("c4Probe", self.0)) {
            call(&target, "notice", vec![record.into()]).await?;
        }
        Ok(None)
    }
}

impl SessionProbe {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        a0: InstanceId,
        a1: mpsc::UnboundedSender<Loaded>,
    ) -> rutis::Result<Self> {
        Ok(Self(a0, a1, names.key()?))
    }
}
impl rutis::host::DynPlugin for SessionProbe {
    fn name(&self) -> &str {
        "C4SessionProbe"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.2.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.2)?;
            let closed = Arc::new(AtomicUsize::new(0));
            ctx.provide_at::<dyn HostDispatch>(
                host_key_in("c4Session", self.0),
                Arc::new(SessionHost(identity.session_id.clone(), closed.clone())),
            )?;
            let session = self.0;
            let probe = Arc::new(BroadcastProbe(session));
            ctx.listen::<SessionBroadcast>()
                .name(session_channel(&identity.session_id))
                .on(move |event, call| {
                    let probe = probe.clone();
                    async move { probe.on_record(call.ctx(), &event.record).await }
                })?;
            self.1
                .send(Loaded {
                    session_id: identity.session_id.clone(),
                    session,
                    ctx: ctx.clone(),
                    closed,
                })
                .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

async fn call(
    service: &Arc<dyn HostDispatch>,
    method: &str,
    args: Vec<Value>,
) -> Result<Json, rutis_bridge::session::Error> {
    let reply = tokio::task::block_in_place(|| service.invoke(method, Value::List(args)))?;
    settle(reply).await?.json()
}
fn service(loaded: &Loaded, name: &str) -> Arc<dyn HostDispatch> {
    loaded
        .ctx
        .get_at(&host_key_in(name, loaded.session))
        .expect("probe service Active")
}
async fn fence(
    loaded: &Loaded,
    probe: &Arc<dyn HostDispatch>,
    token: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let waiting = call(probe, "fence", vec![json!(token).into()]);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut waiting)
            .await
            .is_err(),
        "a fence must stay pending until its notice arrives"
    );
    loaded.ctx.emit(SessionBroadcast::new(
        &loaded.session_id,
        SessionBroadcastEvent::Fence {
            recipient: "c4-spike".into(),
            token,
        },
    ))?;
    tokio::time::timeout(Duration::from_secs(5), waiting).await??;
    Ok(())
}
async fn measure(
    service: &Arc<dyn HostDispatch>,
    method: &str,
    bytes: usize,
) -> Result<Json, Box<dyn std::error::Error>> {
    let payload = json!("x".repeat(bytes));
    for _ in 0..20 {
        assert_eq!(
            call(service, method, vec![payload.clone().into()]).await?,
            payload
        );
    }
    let mut samples = Vec::with_capacity(500);
    for _ in 0..500 {
        let started = Instant::now();
        let result = call(service, method, vec![payload.clone().into()]).await?;
        samples.push(started.elapsed().as_secs_f64() * 1_000_000.0);
        assert_eq!(result, payload);
    }
    samples.sort_by(f64::total_cmp);
    let percentile = |p: usize| samples[(samples.len() * p).div_ceil(100) - 1];
    Ok(
        json!({"method":method,"payloadBytes":bytes,"samples":samples.len(),"p50Us":percentile(50),"p95Us":percentile(95),"p99Us":percentile(99)}),
    )
}
fn pending_call(
    tool: Arc<dyn HostDispatch>,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<Json, rutis_bridge::session::Error>>,
) {
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        let _cancel_on_return = token.clone().drop_guard();
        call(
            &tool,
            "execute",
            vec![
                arg(&ToolCall {
                    id: "held".into(),
                    name: "spike".into(),
                    input: [("hold".into(), json!(true))].into_iter().collect(),
                })?,
                Value::future(async move {
                    token.cancelled().await;
                    Ok(Value::Undefined)
                }),
            ],
        )
        .await
    });
    (cancel, task)
}

async fn run(root: &Ctx) -> Result<Json, Box<dyn std::error::Error>> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let package = workspace.join("runtime/node_modules/@arcships/rutis-runtime");
    let launcher = Launcher::new("node")
        .arg("--import")
        .arg("tsx")
        .arg(package.join("src/runner.mjs"))
        .cwd(&package)
        .inherit_fd()
        .stdin(Stdio::Null)
        .stdout(Stdio::Null);
    let anchor = workspace.join("runtime/package.json");
    let mut builder = AgentBuilder::new(AgentConfig::default());
    for name in ["c4Session", "c4Tool", "c4Probe"] {
        builder
            .catalog_mut()
            .register_shared_instance(name, SESSION_GROUP);
    }
    let (loaded_send, mut loaded) = mpsc::unbounded_channel();
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.require_feature(Feature::new("C4SessionProbe").session(
        "C4SessionProbe",
        move |names| {
            Ok(Box::new(SessionProbe::new(
                names,
                names.instance(SESSION_GROUP)?,
                loaded_send.clone(),
            )?))
        },
    ));
    let mut builtins = Modules::new();
    let node = register_part(&mut builtins, builder.catalog(), move |_| {
        Ok(Box::new(Node {
            launcher: launcher.clone(),
            anchor: anchor.clone(),
        }))
    })?;
    let (anchor_send, mut anchors) = mpsc::unbounded_channel();
    let anchor = register_part(&mut builtins, builder.catalog(), move |names| {
        Ok(Box::new(Anchor::new(names, anchor_send.clone())?))
    })?;
    let plugins = builder.plugin_rows(&mut builtins).await?.rows;
    let mut session_row = builder.install(&mut builtins, []).await?;
    session_row["config"]
        .as_array_mut()
        .expect("Session group config")
        .push(
            json!({"id":"C4SessionPart","name":workspace.join("runtime/spikes/session-part.ts")}),
        );
    let resolver = SessionResolver::npm(builder.catalog(), "node");
    let plugin = LoaderPlugin::new(
        Chain::new().with(builtins).with(resolver.clone()),
        LoaderOptions {
            catalog: builder.catalog().clone(),
            ..Default::default()
        },
    );
    let loader = plugin.handle();
    resolver.attach(&loader);
    (&root.plugin(plugin)).await?;
    resolver.follow_row("node");
    let started = Instant::now();
    let agent_row = builder.agent_row([json!({"id":anchor,"name":anchor}), session_row]);
    let mut inserted = vec![json!({"id":node,"name":node})];
    inserted.extend(plugins);
    inserted.push(agent_row);
    let report = loader
        .reconcile(
            vec![Layer::new(
                "spike",
                serde_json::from_value(json!([{ "insert": inserted }]))?,
            )],
            None,
        )
        .await?;
    assert!(
        report.failures.is_empty() && report.issues.is_empty(),
        "{report:?}"
    );
    let runtime_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let agent_instance = loader.create_instance(root, AGENT_GROUP).await?;
    let anchor = anchors.recv().await.expect("Agent anchor");
    let agent = builder.build(loader.clone(), &anchor);
    let started = Instant::now();
    let one = agent.create_session(SessionConfig::default()).await?;
    let session_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let first = loaded.recv().await.expect("first Session probe");
    let two = agent.create_session(SessionConfig::default()).await?;
    let second = loaded.recv().await.expect("second Session probe");
    assert_eq!(first.session_id, one.id());
    assert_eq!(second.session_id, two.id());
    let probe = service(&first, "c4Probe");
    let other = service(&second, "c4Probe");
    let tool = service(&first, "c4Tool");
    let status = call(&probe, "status", vec![]).await?;
    assert_eq!(status["sessionId"], one.id());
    assert_eq!(call(&other, "status", vec![]).await?["pid"], status["pid"]);
    let declaration: ToolDefinition =
        serde_json::from_value(call(&tool, "declaration", vec![]).await?)?;
    assert_eq!(declaration.name, "spike");
    let result: ToolResult = serde_json::from_value(
        call(
            &tool,
            "execute",
            vec![arg(&ToolCall {
                id: "hello".into(),
                name: "spike".into(),
                input: [("text".into(), json!("hello"))].into_iter().collect(),
            })?],
        )
        .await?,
    )?;
    assert_eq!(result, ToolResult::text(format!("{}:hello", one.id())));
    one.submit(
        "tool:echo {\"text\":\"native flow\"}".into(),
        InputMode::Queue,
        "main",
    )
    .await?;
    one.idle("main").await?;
    fence(&first, &probe, 1).await?;
    fence(&second, &other, 2).await?;
    let first_events = call(&probe, "status", vec![]).await?["notices"]
        .as_array()
        .expect("notices")
        .clone();
    assert!(
        first_events.iter().any(
            |record| record["event"]["type"] == "input" && record["event"]["name"] == "applied"
        )
    );
    assert!(
        call(&other, "status", vec![]).await?["notices"]
            .as_array()
            .expect("other notices")
            .iter()
            .all(
                |record| record["event"]["type"] != "input" || record["event"]["name"] != "applied"
            )
    );
    let (cancel, held) = pending_call(tool.clone());
    call(&probe, "started", vec![]).await?;
    cancel.cancel();
    assert_eq!(held.await??, json!({"content":"cancelled"}));
    let mut measurements = Vec::new();
    for bytes in [64, 4096, 65536] {
        for method in ["sync", "async"] {
            measurements.push(measure(&probe, method, bytes).await?);
        }
    }
    let (_cancel, held) = pending_call(tool.clone());
    call(&probe, "started", vec![]).await?;
    let started = Instant::now();
    one.close().await?;
    assert_eq!(held.await??, json!({"content":"unloaded"}));
    let close_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(first.closed.load(Ordering::SeqCst), 1);
    assert!(call(&tool, "declaration", vec![]).await.is_err());
    assert_eq!(call(&other, "status", vec![]).await?["sessionId"], two.id());
    two.submit("hello".into(), InputMode::Queue, "main").await?;
    two.idle("main").await?;
    two.close().await?;
    assert_eq!(second.closed.load(Ordering::SeqCst), 1);
    agent.close().await?;
    agent_instance
        .view
        .dispose()
        .await
        .map_err(|error| error.to_string())?;
    Ok(
        json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"node":status["node"],"runtimeReconcileMs":runtime_ms,"firstSessionMs":session_ms,"sessionCloseMs":close_ms,"nodePid":status["pid"],"measurements":measurements,"checks":["scoped services","shared runtime","tool DTOs","Session broadcast isolation","explicit cancellation","in-flight unload","stale handler rejection","unaffected native Session"]}),
    )
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Ctx::root_with_sink(
        tokio::runtime::Handle::current(),
        Arc::new(|error| eprintln!("{error}")),
    );
    root.provide(LocalId(PeerId::new("c4-spike")?))?;
    let result = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(60), run(&root)))
        .catch_unwind()
        .await;
    root.shutdown().await.map_err(|error| error.to_string())?;
    let result = result.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    println!("{}", serde_json::to_string_pretty(&result??)?);
    Ok(())
}

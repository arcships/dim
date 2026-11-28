//! Name-bound Rust parts across languages (plugin contract P2, rutis usage
//! §3.5): a Rust Session part provides a crossing name that a TypeScript
//! row reads, and a Rust branch part reads a name that TypeScript row
//! provides. Each Session's parts see their own Session's copies.
use super::session_typescript::{fixture, new_session, setup};
use super::*;
use dim_agent_runtime::{SESSION_GROUP, SessionScope, TimelineContext};
use rutis::host::{CtxHost, Key};
use rutis_bridge::session::{HostDispatch, Reply, Value as RpcValue};
use std::sync::Mutex;

type Log = Arc<Mutex<Vec<String>>>;

/// `testGreeter` of one Session, served to other languages by name.
struct Greeter(String);
impl HostDispatch for Greeter {
    fn invoke(&self, method: &str, _: RpcValue) -> Reply {
        assert_eq!(method, "hello");
        Ok(json!(format!("hello {}", self.0)).into())
    }
    fn methods(&self) -> Option<Value> {
        Some(json!({"hello":"sync"}))
    }
}
struct GreeterPart(Key<dyn HostDispatch>, rutis::host::Key<SessionScope>);
impl GreeterPart {
    fn new(names: &dim_agent_runtime::Names<'_>, a0: Key<dyn HostDispatch>) -> rutis::Result<Self> {
        Ok(Self(a0, names.key()?))
    }
}
impl rutis::host::DynPlugin for GreeterPart {
    fn name(&self) -> &str {
        "test.named-greeter"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.1.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.1)?;
            ctx.provide_at::<dyn HostDispatch>(
                self.0.clone(),
                Arc::new(Greeter(identity.session_id.clone())),
            )?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// Reads the TypeScript row's `namedGreeter` from each branch, at the
/// name's catalog key.
struct MethodsReader {
    log: Log,
    methods: Key<dyn HostDispatch>,
    branch: Key<TimelineContext>,
}
impl MethodsReader {
    fn new(names: &dim_agent_runtime::Names<'_>, log: Log) -> rutis::Result<Self> {
        Ok(Self {
            log,
            methods: names.named("namedGreeter")?,
            branch: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for MethodsReader {
    fn name(&self) -> &str {
        "test.named-methods-reader"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.methods.erase(), self.branch.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let branch = ctx.require_at(&self.branch)?;
            self.log.lock().unwrap().push(format!(
                "{} {} for {}",
                methods.provider().unwrap_or_default(),
                methods.methods().unwrap_or_default(),
                branch.session_id
            ));
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_named_parts_share_services_across_languages_per_session() {
    let log = Log::default();
    let (mut builder, _requests, _release) = setup();
    builder
        .catalog_mut()
        .register_shared_instance("testGreeter", SESSION_GROUP);
    let read = log.clone();
    builder
        .require_feature(Feature::new("test.named-greeter").session(
            "test.named-greeter",
            |names| {
                Ok(Box::new(GreeterPart::new(
                    names,
                    names.named("testGreeter")?,
                )?))
            },
        ))
        .require_feature(
            Feature::new("test.named-methods-reader")
                .branch("test.named-methods-reader", move |names| {
                    Ok(Box::new(MethodsReader::new(names, read.clone())?))
                }),
        );
    let mut parts = fixture();
    let root = parts.anchor.parent().unwrap().join("fixtures");
    parts.rows = vec![json!({
        "id": "named-greeter",
        "name": root.join("named-greeter.ts"),
        "provides": {},
        "instanceNames": {"namedGreeter": "session"},
        "config": {},
    })];
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-np-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        ids.push(new_session(&mut client, json!(["echo"])).await);
    }
    for id in &ids {
        let asked = client
            .rpc("session.greeter.ask", json!({"sessionId":id}))
            .await;
        assert_ok(&asked);
        assert_eq!(asked["result"], json!(format!("hello {id}")));
    }
    let read = log.lock().unwrap().clone();
    for id in &ids {
        assert!(
            read.iter()
                .any(|entry| entry.starts_with("row named-greeter ")
                    && entry.contains("ask")
                    && entry.ends_with(&format!(" for {id}"))),
            "{read:?}"
        );
    }
    assert_eq!(read.len(), 2, "{read:?}");
    server.shutdown();
    server.join().await.unwrap();
}

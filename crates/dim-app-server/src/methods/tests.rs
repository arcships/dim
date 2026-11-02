//! Per-call lookup, provider-owned calls and namespace ownership contracts.
use super::*;
use dim_agent_capabilities::methods::{Handler, Interruption};
use dim_agent_runtime::{Modules, Names, names::register, register_part, wait_active};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use rutis_loader::{Layer, LoaderPlugin, Patch};
use serde_json::json;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

/// Provides one namespace with a fixed handler.
struct Owner {
    name: &'static str,
    namespace: &'static str,
    declarations: Vec<MethodDecl>,
    handler: Handler,
    methods: Key<ManagementMethods>,
}
impl Owner {
    fn new(
        names: &Names<'_>,
        name: &'static str,
        namespace: &'static str,
        declarations: &[&str],
    ) -> rutis::Result<Self> {
        Ok(Self {
            name,
            namespace,
            declarations: declarations
                .iter()
                .map(|method| decl(method, Interruption::Drain))
                .collect(),
            handler: Arc::new(|_, _, _, _| Box::pin(async { Ok(json!({})) })),
            methods: names.key()?,
        })
    }
}
impl DynPlugin for Owner {
    fn name(&self) -> &str {
        self.name
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            methods.register(
                ctx,
                self.namespace,
                self.declarations.clone(),
                self.handler.clone(),
            )?;
            Ok(Effect::Done)
        })
    }
}

/// A provider that stays Pending on a service nobody provides.
struct Missing;
struct Waiting {
    missing: Key<Missing>,
    methods: Key<ManagementMethods>,
}
impl Waiting {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            missing: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for Waiting {
    fn name(&self) -> &str {
        "Waiting"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.missing.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            methods.register(
                ctx,
                "pending",
                vec![decl("pending.read", Interruption::Drain)],
                Arc::new(|_, _, _, _| Box::pin(async { Ok(json!({})) })),
            )?;
            Ok(Effect::Done)
        })
    }
}

/// A provider whose apply fails before it registers `broken`.
struct Broken;
impl Plugin for Broken {
    type Deps = ();
    fn name(&self) -> &str {
        "Broken"
    }
    async fn apply(&self, _: &Ctx, (): ()) -> rutis::Result {
        Err(rutis::Error::from(ErrorKind::PluginFailed(
            "broken owner".into(),
        )))
    }
}

/// Registers `make`'s part as a process row and returns its name.
fn part<P: DynPlugin + 'static>(
    builtins: &mut Modules,
    make: impl Fn(&Names<'_>) -> rutis::Result<P> + Send + Sync + 'static,
) -> String {
    register_part(builtins, &catalog(), move |names| {
        Ok(Box::new(make(names)?))
    })
    .unwrap()
}
fn catalog() -> dim_agent_runtime::names::ServiceCatalog {
    let mut catalog = dim_agent_runtime::names::catalog();
    register::<Missing>(&mut catalog, None);
    register::<ManagementMethods>(&mut catalog, None);
    catalog
}
fn decl(name: &str, interruption: Interruption) -> MethodDecl {
    MethodDecl::new(name, false, interruption)
}
fn rejected(result: Result<Accepted, RpcError>) -> String {
    result.err().unwrap().data.unwrap().app_error
}

/// Owner rows in a real loader, as the process assembles them, and the
/// management table they register with.
struct Assembly {
    root: Ctx,
    loader: Loader,
    table: Arc<ManagementMethods>,
}
impl Assembly {
    /// `rows` registers each row and returns its loader rows; the table's
    /// row comes first.
    async fn new(rows: impl FnOnce(&mut Modules) -> Vec<Value>) -> Self {
        let root = Ctx::root().unwrap();
        let mut builtins = Modules::new();
        let table = register_part(&mut builtins, &catalog(), |names| {
            Ok(Box::new(ManagementMethodsPlugin::new(names)?))
        })
        .unwrap();
        let rows = std::iter::once(row(table))
            .chain(rows(&mut builtins))
            .collect();
        let plugin = LoaderPlugin::new(builtins, Default::default());
        let loader = plugin.handle();
        let view = root.plugin(plugin);
        wait_active(
            &root,
            &[view],
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let assembly = Layer::new(
            "assembly",
            vec![Patch {
                insert: Some(rows),
                ..Default::default()
            }],
        );
        loader.reconcile(vec![assembly], None).await.unwrap();
        let table = root
            .get::<ManagementMethods>()
            .expect("the management table is active");
        Self {
            root,
            loader,
            table,
        }
    }
    fn dispatch(&self, method: &str, entry: &CancellationToken) -> Result<Accepted, RpcError> {
        dispatch(
            &self.table,
            &self.loader,
            method.into(),
            json!({}),
            entry.clone(),
            |_| {},
        )
    }
    fn listed(&self) -> Vec<String> {
        self.table
            .listed()
            .into_iter()
            .map(|decl| decl.name)
            .collect()
    }
    async fn state(&self, row: &str, state: FiberState) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !matches!(
                self.loader.get(row).map(|entry| entry.status),
                Some(EntryStatus::Running(snapshot)) if snapshot.state == state
            ) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{row} did not reach {state:?}"));
    }
}
fn row(name: String) -> Value {
    json!({"id": name, "name": name})
}
fn disabled(name: String) -> Value {
    json!({"id": name, "name": name, "disabled": true})
}

#[derive(Default)]
struct State {
    generation: AtomicUsize,
    committed: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    cancelled: tokio::sync::Notify,
    cancelled_count: AtomicUsize,
    /// Its next generations fail before registering.
    fail_again: std::sync::atomic::AtomicBool,
}
/// `fixture.*` with a blocking Drain write, a Report wait and a Drain read
/// that returns the provider generation.
struct Fixture {
    state: Arc<State>,
    methods: Key<ManagementMethods>,
}
impl Fixture {
    fn new(names: &Names<'_>, state: Arc<State>) -> rutis::Result<Self> {
        Ok(Self {
            state,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for Fixture {
    fn name(&self) -> &str {
        "Fixture"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let state = self.state.clone();
            let generation = state.generation.fetch_add(1, Ordering::SeqCst);
            if generation > 0 && state.fail_again.load(Ordering::SeqCst) {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(
                    "fixture fails again".into(),
                )));
            }
            methods.register(
                ctx,
                "fixture",
                vec![
                    decl("fixture.write", Interruption::Drain),
                    decl("fixture.wait", Interruption::Report),
                    decl("fixture.read", Interruption::Drain),
                ],
                Arc::new(move |method, _, cancel, _| {
                    let state = state.clone();
                    Box::pin(async move {
                        if method == "fixture.write" {
                            state.entered.notify_one();
                            state.release.notified().await;
                            assert!(cancel.is_cancelled());
                            state.committed.fetch_add(1, Ordering::SeqCst);
                        } else if method == "fixture.wait" {
                            state.entered.notify_one();
                            cancel.cancelled().await;
                            state.cancelled_count.fetch_add(1, Ordering::SeqCst);
                            state.cancelled.notify_one();
                        }
                        Ok(json!(generation))
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_interrupts_report_calls_and_waits_for_drain_calls() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(State::default());
        let fixture = state.clone();
        let assembly = Assembly::new(|builtins| {
            vec![row(part(builtins, move |names| {
                Fixture::new(names, fixture.clone())
            }))]
        })
        .await;
        assembly.state("Fixture", FiberState::Active).await;
        let entry = CancellationToken::new();
        let write = assembly.dispatch("fixture.write", &entry).unwrap();
        state.entered.notified().await;
        let wait = assembly.dispatch("fixture.wait", &entry).unwrap();
        state.entered.notified().await;
        let loader = assembly.loader.clone();
        let restart = tokio::spawn(async move { loader.restart("Fixture").await.unwrap() });
        let interrupted = wait.reply.await.unwrap().unwrap_err();
        assert_eq!(interrupted.data.unwrap().app_error, "operation_interrupted");
        assert!(interrupted.message.contains("may already have committed"));
        state.cancelled.notified().await;
        assert!(
            !restart.is_finished(),
            "an accepted drain call holds its provider's unload"
        );
        assert_eq!(
            rejected(assembly.dispatch("fixture.read", &entry)),
            "service_reloading"
        );
        state.release.notify_one();
        assert_eq!(write.reply.await.unwrap().unwrap(), json!(0));
        restart.await.unwrap();
        assembly.state("Fixture", FiberState::Active).await;
        assert_eq!(state.committed.load(Ordering::SeqCst), 1);
        assert_eq!(
            assembly
                .dispatch("fixture.read", &entry)
                .unwrap()
                .reply
                .await
                .unwrap()
                .unwrap(),
            json!(1)
        );
        assembly.root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_caller_unload_interrupts_report_calls_and_an_inactive_provider_is_isolated() {
    let state = Arc::new(State::default());
    let fixture = state.clone();
    let assembly = Assembly::new(|builtins| {
        vec![
            row(part(builtins, move |names| {
                Fixture::new(names, fixture.clone())
            })),
            row(part(builtins, Waiting::new)),
            row(part(builtins, |_| Ok(Broken))),
            disabled(part(builtins, |names| {
                Owner::new(names, "Off", "off", &["off.read"])
            })),
            row(part(builtins, |names| {
                Owner::new(names, "Other", "other", &["other.read"])
            })),
            json!({"id": "Unresolvable", "name": "unregistered"}),
        ]
    })
    .await;
    assembly.state("Waiting", FiberState::Pending).await;
    assembly.state("Broken", FiberState::Failed).await;
    assembly.state("Other", FiberState::Active).await;
    assembly.state("Fixture", FiberState::Active).await;
    assert!(matches!(
        assembly
            .loader
            .get("Unresolvable")
            .map(|entry| entry.status),
        Some(EntryStatus::Unresolved(_))
    ));
    let entry = CancellationToken::new();
    for method in [
        "pending.read",
        "broken.read",
        "unresolvable.read",
        "off.read",
        "absent.read",
    ] {
        assert_eq!(
            rejected(assembly.dispatch(method, &entry)),
            "unknown_method",
            "{method}"
        );
    }
    assembly
        .dispatch("other.read", &entry)
        .unwrap()
        .reply
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        assembly.listed(),
        [
            "fixture.read",
            "fixture.wait",
            "fixture.write",
            "other.read"
        ]
    );
    let entry = CancellationToken::new();
    let wait = assembly.dispatch("fixture.wait", &entry).unwrap();
    state.entered.notified().await;
    entry.cancel();
    assert_eq!(
        wait.reply
            .await
            .unwrap()
            .unwrap_err()
            .data
            .unwrap()
            .app_error,
        "operation_interrupted"
    );
    let _ = wait.settled.await;
    assert_eq!(state.cancelled_count.load(Ordering::SeqCst), 1);
    state.fail_again.store(true, Ordering::SeqCst);
    let _ = assembly.loader.restart("Fixture").await;
    assembly.state("Fixture", FiberState::Failed).await;
    let error = assembly.dispatch("fixture.read", &entry).err().unwrap();
    assert_eq!(error.code, -32000);
    assert_eq!(error.data.unwrap().app_error, "service_unavailable");
    assembly.root.shutdown().await.unwrap();
}

#[tokio::test]
async fn namespace_validation_and_nested_ownership_follow_the_longest_provided_namespace() {
    let root = Ctx::root().unwrap();
    let table = root.plugin(ManagementMethodsPlugin { table: Key::of() });
    let cancellation = CancellationToken::new();
    wait_active(&root, &[table], Duration::from_secs(5), &cancellation)
        .await
        .unwrap();
    for namespace in [
        "hello.nested",
        "daemon.extra",
        "agent.extra",
        "approval.extra",
        "session",
        "session.event",
        "session.goal",
        "session.mcp.extra",
        "session.mcp.app.widget",
        "session.mcp.app.call",
        "session.mcp.app",
        "session.create.extra",
    ] {
        let read = format!("{namespace}.read");
        let catalog = catalog();
        let names = Names::check(&catalog, &[], None);
        let invalid =
            root.plugin(Owner::new(&names, "Owner", namespace, &[read.as_str()]).unwrap());
        assert!(
            wait_active(&root, &[invalid], Duration::from_secs(5), &cancellation)
                .await
                .is_err(),
            "{namespace}"
        );
    }
    root.shutdown().await.unwrap();

    let parent = |names: &Names<'_>| {
        Owner::new(
            names,
            "Parent",
            "fixture",
            &["fixture.read", "fixture.nested.read"],
        )
    };
    let child =
        |names: &Names<'_>| Owner::new(names, "Child", "fixture.nested", &["fixture.nested.other"]);
    let entry = CancellationToken::new();
    for child_enabled in [true, false] {
        let assembly = Assembly::new(|builtins| {
            let child = part(builtins, child);
            vec![
                row(part(builtins, parent)),
                if child_enabled {
                    row(child)
                } else {
                    disabled(child)
                },
            ]
        })
        .await;
        assembly.state("Parent", FiberState::Active).await;
        assembly
            .dispatch("fixture.read", &entry)
            .unwrap()
            .reply
            .await
            .unwrap()
            .unwrap();
        if child_enabled {
            assembly.state("Child", FiberState::Active).await;
            assert_eq!(
                rejected(assembly.dispatch("fixture.nested.read", &entry)),
                "unknown_method"
            );
            assembly
                .dispatch("fixture.nested.other", &entry)
                .unwrap()
                .reply
                .await
                .unwrap()
                .unwrap();
            assert_eq!(assembly.listed(), ["fixture.nested.other", "fixture.read"]);
        } else {
            assembly
                .dispatch("fixture.nested.read", &entry)
                .unwrap()
                .reply
                .await
                .unwrap()
                .unwrap();
            assert_eq!(assembly.listed(), ["fixture.nested.read", "fixture.read"]);
            assert_eq!(
                rejected(assembly.dispatch("fixture.nested.other", &entry)),
                "unknown_method"
            );
        }
        let long = format!("fixture.{}read", "a.".repeat(1 << 17));
        let started = std::time::Instant::now();
        for method in [
            "fixture.Read",
            "fixture..read",
            "fixture.read.",
            long.as_str(),
        ] {
            assert_eq!(
                rejected(assembly.dispatch(method, &entry)),
                "unknown_method"
            );
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assembly.root.shutdown().await.unwrap();
    }
}

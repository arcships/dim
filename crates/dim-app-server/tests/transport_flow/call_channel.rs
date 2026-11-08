//! Connection-bound notifications (methods contract A.2): `callChannel`
//! `notify` and `closed` as the Rust binding delivers them, through both
//! method tables, with ordering, refusal and lifetime.
use super::*;
use dim_agent_capabilities::methods::{
    Caller, Interruption, MethodCall, MethodDecl, NotifyError, RpcError, SessionMethods,
};
use dim_agent_runtime::Feature;
use dim_app_server::methods::ManagementMethods;
use rutis::host::{BoxFuture, DynPlugin, Effect, Key, TypeKey};
use serde_json::Map;

/// What the provider of a test records between calls.
struct ChannelState {
    /// The caller of the last call that kept one.
    caller: std::sync::Mutex<Option<Caller>>,
    /// A held caller's `closed` resolved.
    closed: Arc<tokio::sync::Notify>,
    /// Releases `channel.stream`'s task, which notifies after the reply.
    release: Arc<tokio::sync::Notify>,
    /// `channel.fill` finished: the first code the full allowance returned
    /// and the code of one more notification after it.
    filled: std::sync::Mutex<Option<(&'static str, &'static str)>>,
    /// `channel.hold` calls that reached their wait, and the gate that
    /// lets them reply.
    holding: AtomicUsize,
    holding_changed: Arc<tokio::sync::Notify>,
    release_held: Arc<tokio::sync::Semaphore>,
    /// The Session provider generations mounted so far, the newest last.
    views: std::sync::Mutex<Vec<rutis::host::FiberView>>,
}
impl Default for ChannelState {
    fn default() -> Self {
        Self {
            caller: std::sync::Mutex::new(None),
            closed: Arc::default(),
            release: Arc::default(),
            filled: std::sync::Mutex::new(None),
            holding: AtomicUsize::new(0),
            holding_changed: Arc::default(),
            release_held: Arc::new(tokio::sync::Semaphore::new(0)),
            views: std::sync::Mutex::new(Vec::new()),
        }
    }
}
impl ChannelState {
    fn caller(&self) -> Caller {
        self.caller
            .lock()
            .unwrap()
            .clone()
            .expect("a call kept its caller")
    }
    fn code(&self, method: &str, params: Map<String, Value>) -> Value {
        match self.caller().notify(method, params) {
            Ok(()) => json!({"ok": true}),
            Err(error) => json!({"code": error.code()}),
        }
    }
    /// Restarts the Session provider's generation of the newest Session.
    async fn restart(&self) {
        let view = self
            .views
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a Session provider generation");
        view.restart().await.unwrap();
    }
}

/// A declaration with the notifications it declares; a notification belongs
/// to the provider generation, so each name is declared once.
fn notification(name: &str, notifications: &[&str]) -> MethodDecl {
    MethodDecl {
        notifications: notifications.iter().map(|name| (*name).into()).collect(),
        ..MethodDecl::new(name, false, Interruption::Drain)
    }
}

/// The handler both test providers serve: `stream` sends one notification
/// inside the call and one from a task after it, `start` keeps the caller,
/// `fill` holds more notifications than the connection's held allowance,
/// `hold` holds a fixed number until the test releases it, and `attempt`
/// sends through the kept one.
async fn serve(call: MethodCall, state: Arc<ChannelState>) -> Result<Value, RpcError> {
    let caller = call
        .origin
        .caller
        .ok_or_else(|| RpcError::wire(-32603, "no caller"))?;
    let session = call.method.starts_with("session.");
    let names = match session {
        true => ("session.channel.first", "session.channel.second"),
        false => ("channel.first", "channel.second"),
    };
    match call.method.as_str() {
        "channel.stream" | "session.channel.stream" => {
            *state.caller.lock().unwrap() = Some(caller.clone());
            let first = caller.notify(names.0, map(json!({"step": 1})));
            let release = state.release.clone();
            tokio::spawn(async move {
                release.notified().await;
                let _ = caller.notify(names.1, map(json!({"step": 2})));
            });
            Ok(match first {
                Ok(()) => json!({"ok": true}),
                Err(error) => json!({"code": error.code()}),
            })
        }
        "channel.start" | "session.channel.start" => {
            *state.caller.lock().unwrap() = Some(caller.clone());
            if call.params["hold"] == true {
                let closed = caller.closed();
                let state = state.clone();
                tokio::spawn(async move {
                    closed.await;
                    state.closed.notify_one();
                });
            }
            Ok(json!({}))
        }
        "channel.fill" | "session.channel.fill" => {
            let count = call.params["count"].as_u64().unwrap_or(0) as usize;
            let mut first = "ok";
            for step in 0..count {
                if let Err(error) = caller.notify(names.0, map(json!({"step": step}))) {
                    first = error.code();
                    break;
                }
            }
            let later = match caller.notify(names.0, map(json!({"late": true}))) {
                Ok(()) => "ok",
                Err(error) => error.code(),
            };
            *state.filled.lock().unwrap() = Some((first, later));
            Ok(json!({}))
        }
        "channel.hold" => {
            let count = call.params["count"].as_u64().unwrap_or(0) as usize;
            let tag = call.params["tag"].as_str().unwrap_or_default().to_owned();
            for step in 0..count {
                caller
                    .notify(names.0, map(json!({"tag": tag, "step": step})))
                    .map_err(|error| RpcError::service(error.code(), "notification refused"))?;
            }
            state.holding.fetch_add(1, Ordering::SeqCst);
            state.holding_changed.notify_waiters();
            let _permit = state
                .release_held
                .acquire()
                .await
                .map_err(|_| RpcError::wire(-32603, "the held call's gate closed"))?;
            Ok(json!({"tag": tag}))
        }
        "channel.attempt" | "session.channel.attempt" => {
            let name = call.params["method"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let bytes = call.params["bytes"].as_u64().unwrap_or(0) as usize;
            let mut params = Map::new();
            if bytes > 0 {
                params.insert("text".into(), json!("x".repeat(bytes)));
            }
            Ok(state.code(&name, params))
        }
        _ => Err(RpcError::unknown_method()),
    }
}

fn map(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Processes the management namespace `channel`.
struct ChannelMethods {
    table: Key<ManagementMethods>,
    state: Arc<ChannelState>,
}
impl DynPlugin for ChannelMethods {
    fn name(&self) -> &str {
        "channel-methods-fixture"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.table.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let state = self.state.clone();
            ctx.require_at(&self.table)?.register(
                ctx,
                "channel",
                vec![
                    notification(
                        "channel.stream",
                        &["channel.first", "channel.second", "channel.other"],
                    ),
                    notification("channel.start", &[]),
                    notification("channel.attempt", &[]),
                    notification("channel.fill", &[]),
                    notification("channel.hold", &[]),
                ],
                Arc::new(move |call, _, _| {
                    let state = state.clone();
                    Box::pin(async move { serve(call, state).await })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

/// Mounts the Session namespace's provider as a child, so the test can
/// reload its generation while the Session lives.
struct ChannelSessionOwner {
    methods: Key<SessionMethods>,
    state: Arc<ChannelState>,
}
impl DynPlugin for ChannelSessionOwner {
    fn name(&self) -> &str {
        "channel-session-owner-fixture"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let view = dim_agent_runtime::mount_active(
                ctx,
                ChannelSession {
                    methods,
                    state: self.state.clone(),
                },
            )
            .await?;
            self.state.views.lock().unwrap().push(view);
            Ok(Effect::Done)
        })
    }
}

/// Serves the Session namespace `session.channel`.
struct ChannelSession {
    methods: Arc<SessionMethods>,
    state: Arc<ChannelState>,
}
impl DynPlugin for ChannelSession {
    fn name(&self) -> &str {
        "channel-session-fixture"
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let state = self.state.clone();
            self.methods.register(
                ctx,
                "session.channel",
                vec![
                    notification(
                        "session.channel.stream",
                        &["session.channel.first", "session.channel.second"],
                    ),
                    notification("session.channel.start", &[]),
                    notification("session.channel.attempt", &[]),
                    notification("session.channel.fill", &[]),
                ],
                Arc::new(move |call, _, _| {
                    let state = state.clone();
                    Box::pin(async move { serve(call, state).await })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

fn channel_plugin(
    state: Arc<ChannelState>,
) -> impl FnOnce(&mut AgentBuilder) + Send + Sync + 'static {
    move |builder| {
        let management = state.clone();
        builder.plugin(move |names| {
            Ok(Box::new(ChannelMethods {
                table: names.key()?,
                state: management.clone(),
            }) as Box<dyn DynPlugin>)
        });
        builder.require_feature(Feature::new("ChannelSession").session(
            "ChannelSession",
            move |names| {
                Ok(Box::new(ChannelSessionOwner {
                    methods: names.key()?,
                    state: state.clone(),
                }))
            },
        ));
    }
}

/// Launches with the channel providers and returns one v3 client.
async fn launch_channel() -> (RunningServer, Client, Arc<ChannelState>) {
    let state = Arc::new(ChannelState::default());
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig::default(),
        channel_plugin(state.clone()),
    )
    .await;
    let client = Client::new_version(&server, false, 3).await;
    (server, client, state)
}

/// A.2: a notification sent while the call runs is held and written right
/// after its reply; one sent later follows in order. Only the calling
/// connection receives them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notifications_follow_the_reply_in_order_on_the_calling_connection_only() {
    let (server, mut client, state) = launch_channel().await;
    let mut other = Client::new_version(&server, false, 3).await;
    let id = client.send("channel.stream", json!({})).await;
    let reply = client.read("the reply").await;
    assert_eq!(reply["id"], id, "{reply}");
    assert_eq!(reply["result"], json!({"ok": true}));
    let first = client.read("the first notification").await;
    assert_eq!(first["jsonrpc"], "2.0");
    assert_eq!(first["method"], "channel.first");
    assert_eq!(first["params"], json!({"step": 1}));
    state.release.notify_one();
    let second = client.read("the second notification").await;
    assert_eq!(second["method"], "channel.second");
    assert_eq!(second["params"], json!({"step": 2}));
    assert_ok(&other.rpc("daemon.ping", json!({})).await);
    assert!(
        other.notices.is_empty(),
        "another connection received the notifications: {:?}",
        other.notices
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2: a name the provider generation did not declare is `Undeclared` and
/// nothing is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_undeclared_notification_name_writes_nothing() {
    let (server, mut client, _) = launch_channel().await;
    assert_ok(&client.rpc("channel.start", json!({})).await);
    let reply = client
        .rpc("channel.attempt", json!({"method": "channel.undeclared"}))
        .await;
    assert_eq!(reply["result"], json!({"code": "undeclared"}), "{reply}");
    assert_ok(&client.rpc("daemon.ping", json!({})).await);
    assert!(
        client.notices.is_empty(),
        "an undeclared name was written: {:?}",
        client.notices
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2 acceptance 6: a caller's disconnect resolves `closed()` and later
/// `notify` calls answer `Gone`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disconnect_resolves_closed_and_later_notifications_answer_gone() {
    let (server, mut client, state) = launch_channel().await;
    assert_ok(&client.rpc("channel.start", json!({"hold": true})).await);
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(10), state.closed.notified())
        .await
        .expect("the caller's closed did not resolve");
    let mut other = Client::new_version(&server, false, 3).await;
    let reply = other
        .rpc("channel.attempt", json!({"method": "channel.first"}))
        .await;
    assert_eq!(reply["result"], json!({"code": "gone"}), "{reply}");
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2: an Rpc reload resolves `closed()` and later `notify` calls answer
/// `Gone` (conflict table, "Rpc reload with calls in flight").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_rpc_reload_resolves_closed_and_later_notifications_answer_gone() {
    let (server, mut client, state) = launch_channel().await;
    assert_ok(&client.rpc("channel.start", json!({"hold": true})).await);
    server.restart("Rpc").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), state.closed.notified())
        .await
        .expect("the caller's closed did not resolve");
    let mut other = Client::new_version(&server, false, 3).await;
    let reply = other
        .rpc("channel.attempt", json!({"method": "channel.first"}))
        .await;
    assert_eq!(reply["result"], json!({"code": "gone"}), "{reply}");
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2: an encoded notification over the frame limit is `TooLarge`; nothing
/// is written and the connection stays open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_notification_is_refused_and_the_connection_stays_open() {
    let (server, mut client, _) = launch_channel().await;
    assert_ok(&client.rpc("channel.start", json!({})).await);
    let reply = client
        .rpc(
            "channel.attempt",
            json!({"method": "channel.first", "bytes": 16 * 1024 * 1024}),
        )
        .await;
    assert_eq!(reply["result"], json!({"code": "too_large"}), "{reply}");
    assert_ok(&client.rpc("daemon.ping", json!({})).await);
    assert!(
        client.notices.is_empty(),
        "an oversized notification was written: {:?}",
        client.notices.len()
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2: a method a Session namespace serves gets the same caller and
/// delivers the same way, to the calling connection only: another client of
/// the same Session receives nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_namespace_method_delivers_connection_bound_notifications() {
    let (server, mut client, state) = launch_channel().await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap();
    let mut other = Client::new_version(&server, false, 3).await;
    assert_ok(
        &other
            .rpc("session.attach", json!({"sessionId": session}))
            .await,
    );
    other.notices.clear();
    let id = client
        .send("session.channel.stream", json!({"sessionId": session}))
        .await;
    let reply = client.read("the Session method reply").await;
    assert_eq!(reply["id"], id, "{reply}");
    assert_eq!(reply["result"], json!({"ok": true}));
    let first = client.read("the Session notification").await;
    assert_eq!(first["method"], "session.channel.first");
    state.release.notify_one();
    let second = client.read("the later Session notification").await;
    assert_eq!(second["method"], "session.channel.second");
    assert_ok(&other.rpc("daemon.ping", json!({})).await);
    let leaked: Vec<_> = other
        .notices
        .iter()
        .filter(|notice| {
            notice["method"] == "session.channel.first"
                || notice["method"] == "session.channel.second"
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "another client of the Session received the notifications: {leaked:?}"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2: the notifications one call holds are bounded by the connection's
/// reliable allowance. A call that holds past it is a slow consumer: the
/// connection closes, the reply is not written, and later notifications on
/// the same caller answer `gone`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_holding_past_the_allowance_closes_the_connection_as_a_slow_consumer() {
    let (server, mut client, state) = launch_channel().await;
    assert_ok(&client.rpc("channel.start", json!({})).await);
    client.send("channel.fill", json!({"count": 1100})).await;
    let Transport::Unix(stream) = &mut client.transport else {
        panic!("the channel fixture is served on the local entry");
    };
    let mut line = String::new();
    let read = tokio::time::timeout(Duration::from_secs(10), stream.read_line(&mut line))
        .await
        .expect("the slow consumer's connection closes");
    assert_eq!(
        read.unwrap(),
        0,
        "the connection stayed open and wrote: {line}"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some((first, later)) = *state.filled.lock().unwrap() {
            assert_eq!(
                (first, later),
                ("gone", "gone"),
                "the first refusal and every later notification answer gone"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the call never reached the allowance's limit"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2: a retained native caller answers `gone` once its provider generation
/// unloads, as the foreign binding's index entry does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_caller_after_its_generation_unloads_answers_gone() {
    let (server, mut client, state) = launch_channel().await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc("session.channel.start", json!({"sessionId": session}))
            .await,
    );
    state.restart().await;
    let reply = client
        .rpc(
            "session.channel.attempt",
            json!({"sessionId": session, "method": "session.channel.first"}),
        )
        .await;
    assert_eq!(reply["result"], json!({"code": "gone"}), "{reply}");
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2: a closed connection does not hide a name the loaded generation did
/// not declare: `undeclared` is decided before the connection's state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_connection_does_not_hide_an_undeclared_name() {
    let (server, mut client, state) = launch_channel().await;
    assert_ok(&client.rpc("channel.start", json!({"hold": true})).await);
    drop(client);
    tokio::time::timeout(Duration::from_secs(10), state.closed.notified())
        .await
        .expect("the caller's closed did not resolve");
    let mut other = Client::new_version(&server, false, 3).await;
    let undeclared = other
        .rpc("channel.attempt", json!({"method": "channel.undeclared"}))
        .await;
    assert_eq!(
        undeclared["result"],
        json!({"code": "undeclared"}),
        "{undeclared}"
    );
    let declared = other
        .rpc("channel.attempt", json!({"method": "channel.first"}))
        .await;
    assert_eq!(declared["result"], json!({"code": "gone"}), "{declared}");
    server.shutdown();
    server.join().await.unwrap();
}

/// A.2: the notifications a call holds have their own allowance, so two
/// unfinished calls holding 512 each never take a reply's room. Both replies
/// arrive, each call's notifications follow its reply in order, and the
/// connection stays open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_notifications_do_not_take_the_replys_room() {
    let (server, mut client, state) = launch_channel().await;
    let a = client
        .send("channel.hold", json!({"count": 512, "tag": "a"}))
        .await;
    let b = client
        .send("channel.hold", json!({"count": 512, "tag": "b"}))
        .await;
    // Both calls hold their frames before either reply is written. The waiter
    // is enabled before the count is read, so a hold that lands between the
    // read and the await cannot be missed: `notify_waiters` stores no permit.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let notified = state.holding_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if state.holding.load(Ordering::SeqCst) >= 2 {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("both calls hold their notifications before their replies");
    state.release_held.add_permits(2);
    let mut replied: Vec<u64> = Vec::new();
    let mut steps: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut notes = 0;
    while notes < 1024 {
        let value = client.read("a reply or a held notification").await;
        if let Some(id) = value["id"].as_u64() {
            assert!(id == a || id == b, "{value}");
            assert!(!replied.contains(&id), "a reply arrived twice: {value}");
            replied.push(id);
            continue;
        }
        assert_eq!(value["method"], "channel.first", "{value}");
        let tag = value["params"]["tag"].as_str().unwrap_or_default();
        let step = value["params"]["step"].as_u64().unwrap_or_default();
        let tag_reply = replied
            .iter()
            .any(|id| *id == a && tag == "a" || *id == b && tag == "b");
        assert!(
            tag_reply,
            "a notification arrived before its own reply: {value}"
        );
        let next = steps.entry(tag.to_owned()).or_insert(0);
        assert_eq!(
            step, *next,
            "a call's notifications are out of order: {value}"
        );
        *next += 1;
        notes += 1;
    }
    assert_eq!(replied.len(), 2, "both calls reply");
    assert!(
        steps.values().all(|next| *next == 512),
        "each call's notifications all arrive: {steps:?}"
    );
    assert_ok(&client.rpc("daemon.ping", json!({})).await);
    server.shutdown();
    server.join().await.unwrap();
}

/// The error a provider sees is the contract's; kept here so the test checks
/// the enum rather than only its code.
#[test]
fn notify_error_codes_are_the_contracts() {
    assert_eq!(NotifyError::Undeclared.code(), "undeclared");
    assert_eq!(NotifyError::Gone.code(), "gone");
    assert_eq!(NotifyError::TooLarge.code(), "too_large");
}

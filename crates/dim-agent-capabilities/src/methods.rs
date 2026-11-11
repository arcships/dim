//! The Rust binding shared by method contributions (methods contract A.1,
//! Appendix F): declarations, the handler, wire errors, the call admission
//! and the method table that both `ManagementMethods` (App Server, process
//! scope) and `SessionMethods` (Session scope) build on, so the two cannot
//! diverge. A part registers a namespace with its owner's table; the
//! registration is withdrawn when the part unloads
//! ([rutis usage R6](../../../docs/design/rutis-usage.md#r6-contributions-register-with-their-owner)).
use dim_agent_runtime::{AgentError, Names, Registrations};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, InstanceId, Key, TypeKey};
use rutis::{Ctx, Error};
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Reference, Reply, Value as BridgeValue, host_key_in, settle,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{Arc, Mutex, Weak},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// A JSON-RPC error as the client receives it; `data.appError` is the stable
/// application code.
#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorData {
    pub app_error: String,
    /// What a refused `daemon.shutdown {ifIdle}` found in use; boxed so a
    /// wire error stays small.
    #[serde(flatten)]
    pub daemon_busy: Option<Box<DaemonBusy>>,
}
/// The counts of a `daemon_busy` refusal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonBusy {
    pub connections: usize,
    pub attached_sessions: usize,
    pub running: usize,
}
impl RpcError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::app(-32602, "invalid_params", message)
    }
    pub fn app(code: i32, app_error: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(ErrorData {
                app_error: app_error.into(),
                daemon_busy: None,
            }),
        }
    }
    /// `daemon.shutdown {ifIdle:true}` found the daemon in use: other client
    /// connections, Sessions with an attached client, and running turns.
    pub fn daemon_busy(connections: usize, attached_sessions: usize, running: usize) -> Self {
        Self {
            code: -32000,
            message: "The App Server is in use".into(),
            data: Some(ErrorData {
                app_error: "daemon_busy".into(),
                daemon_busy: Some(Box::new(DaemonBusy {
                    connections,
                    attached_sessions,
                    running,
                })),
            }),
        }
    }
    pub fn wire(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    /// The server service is reloading; the request did not run and may be retried.
    pub fn reloading() -> Self {
        Self::app(
            -32000,
            "service_reloading",
            "Server service is reloading; retry",
        )
    }
    /// No owner serves the name.
    pub fn unknown_method() -> Self {
        Self::app(-32601, "unknown_method", "Unknown method")
    }
    /// An admitted operation was interrupted by an unload; it may already
    /// have committed.
    pub fn interrupted() -> Self {
        Self::app(
            -32000,
            "operation_interrupted",
            "Service reloaded; the operation may already have committed. Read the current state before retrying",
        )
    }
    pub fn is_closed(&self) -> bool {
        self.data
            .as_ref()
            .is_some_and(|data| data.app_error == "session_closed")
    }
    pub fn closed() -> Self {
        Self::app(
            -32000,
            "session_closed",
            "Session is closed or does not exist",
        )
    }
    /// A service error, `{code, message}`, as the client receives it: the
    /// code is the application code and Rpc chooses the JSON-RPC code
    /// (plugin contract R3).
    pub fn service(code: &str, message: impl Into<String>) -> Self {
        let wire = match code {
            "invalid_params" => -32602,
            "unknown_method" => -32601,
            _ => -32000,
        };
        Self::app(wire, code, message)
    }
    /// The native `AgentError` named by its variant, with its message.
    fn agent(variant: &str, message: String) -> Self {
        match variant {
            "InvalidInput" | "UnknownBranch" => Self::app(-32000, "session_invalid_input", message),
            "Closed" => Self::app(-32000, "session_closed", message),
            "Build" => Self::app(
                -32000,
                "session_build_failed",
                "Session initialization failed",
            ),
            "Reloading" => Self::reloading(),
            "AmbiguousSubmission" => Self::app(
                -32000,
                "submission_ambiguous",
                "Submission identity belongs to multiple inputs",
            ),
            "BranchEnded" => Self::app(-32000, "branch_ended", message),
            _ => Self::wire(-32603, "Session operation failed"),
        }
    }
}
impl From<AgentError> for RpcError {
    fn from(error: AgentError) -> Self {
        let variant = match &error {
            AgentError::InvalidInput(_) => "InvalidInput",
            AgentError::UnknownBranch(_) => "UnknownBranch",
            AgentError::Closed(_) => "Closed",
            AgentError::Build(_) => "Build",
            AgentError::Reloading(_) => "Reloading",
            AgentError::AmbiguousSubmission(_) => "AmbiguousSubmission",
            AgentError::BranchEnded(_) => "BranchEnded",
            _ => "",
        };
        Self::agent(variant, error.to_string())
    }
}

/// Decodes a method's raw params at the wire boundary.
pub fn params<T: DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|_| RpcError::invalid("Invalid request parameters"))
}

/// Encodes a method's reply.
pub fn reply(value: impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|_| RpcError::wire(-32603, "Method reply serialization failed"))
}

/// The name grammar of namespaces and methods (A.1): dot-separated segments
/// `[a-z][a-z0-9_]*`, at most 128 bytes.
pub fn valid_name(name: &str) -> bool {
    name.len() <= 128
        && name.split('.').all(|part| {
            part.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// Whether `name` equals `namespace` or lies inside it.
pub fn within(name: &str, namespace: &str) -> bool {
    name == namespace
        || name
            .strip_prefix(namespace)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

/// One admitted call as its handler receives it (methods contract A.1,
/// `MethodCall`): the declared name, the raw params and who called.
#[derive(Clone)]
pub struct MethodCall {
    pub method: String,
    pub params: Value,
    pub origin: Origin,
}

/// Who called (methods contract A.1, `origin`): the connection that made the
/// call, when a client connection did. An in-process call, and a call a test
/// admits without a connection, has no caller.
#[derive(Clone, Default)]
pub struct Origin {
    pub caller: Option<Caller>,
}

/// Serves one call: `(call, cancellation token, progress)`. The token fires
/// when the provider's generation or the calling load unloads.
pub type Handler = Arc<
    dyn Fn(MethodCall, CancellationToken, Progress) -> BoxFuture<'static, Result<Value, RpcError>>
        + Send
        + Sync,
>;

type Sink = Box<dyn Fn(Value) + Send + Sync>;

/// Why a connection-bound notification was not written (methods contract
/// A.2). `Undeclared` is a name the provider generation did not declare,
/// `Gone` a connection that has closed or a generation or load that has
/// unloaded, and `TooLarge` an encoded notification over the frame limit.
/// An unloaded generation answers `Gone` for every name; while it is loaded,
/// `Undeclared` is decided before the connection's state, in both bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyError {
    Undeclared,
    Gone,
    TooLarge,
}
impl NotifyError {
    /// The stable code a foreign caller reads.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Undeclared => "undeclared",
            Self::Gone => "gone",
            Self::TooLarge => "too_large",
        }
    }
}
impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// The host side of the connection a call arrived on; Rpc implements it. A
/// `Caller` holds no strong reference to the connection, so a provider that
/// keeps one does not keep a closed connection alive.
pub trait CallerConnection: Send + Sync + 'static {
    /// The contract's `connectionId`.
    fn id(&self) -> &str;
    /// Writes the notification to this connection only, in order with
    /// replies and other notices.
    fn notify(
        &self,
        method: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<(), NotifyError>;
    /// Resolves once the connection closes or the Rpc load that admitted the
    /// call unloads.
    fn closed(&self) -> BoxFuture<'static, ()>;
}

/// The Rust binding of `callChannel` (methods contract A.2): the calling
/// connection of one admitted call, bound to the notifications the serving
/// provider generation declared. The provider may keep it after the call
/// returns.
#[derive(Clone)]
pub struct Caller(Arc<CallerInner>);

struct CallerInner {
    connection: Arc<dyn CallerConnection>,
    notifications: Arc<HashSet<String>>,
    /// The serving generation's stopping signal: after it fires, a
    /// notification is `Gone`, as it is for the foreign binding, whose
    /// entries the same unload removes (A.2).
    stopping: CancellationToken,
    /// The foreign binding's `callId` of the call that produced this caller;
    /// `None` for a native provider, which never records into [`Callers`].
    call: Option<String>,
}
impl Caller {
    /// The contract's `connectionId`.
    pub fn id(&self) -> &str {
        self.0.connection.id()
    }
    /// The foreign binding's `callId` of this call. A part of another
    /// language sends its notifications with it so they follow its own
    /// reply, not a later call's.
    pub(crate) fn call_id(&self) -> Option<&str> {
        self.0.call.as_deref()
    }
    /// Sends a connection-bound notification. A generation that has unloaded
    /// is `Gone` for every name; while it is loaded, a name it did not
    /// declare is `Undeclared` and nothing is written, whatever the
    /// connection's state; a closed connection is `Gone`; an encoded
    /// notification over the frame limit is `TooLarge` and the connection
    /// stays open.
    pub fn notify(
        &self,
        method: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<(), NotifyError> {
        if self.0.stopping.is_cancelled() {
            return Err(NotifyError::Gone);
        }
        if !self.0.notifications.contains(method) {
            return Err(NotifyError::Undeclared);
        }
        self.0.connection.notify(method, params)
    }
    /// Resolves once the caller's connection closes or the calling load
    /// unloads (methods contract A.2, `closed`).
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        self.0.connection.closed()
    }
}

/// The client connection a call arrived on, its direct writer, and the
/// process's caller index: what lets a part of another language name a call
/// and a connection long after the call returned.
#[derive(Clone)]
pub struct CallSite {
    /// This call's channel: notifications sent before its reply are held.
    pub connection: Arc<dyn CallerConnection>,
    /// The same connection without this call's hold, for a notification whose
    /// call has finished.
    pub direct: Arc<dyn CallerConnection>,
    pub callers: Arc<Callers>,
    /// The calling connection's close signal, cancelled before the index
    /// forgets the connection: a call admitted in that window records
    /// nothing.
    pub closed: CancellationToken,
}

/// One row instance's calls on one connection: the connection's direct
/// writer and the calls whose replies are not written yet.
struct ConnectionEntry {
    connection: Arc<dyn CallerConnection>,
    calls: HashMap<String, Weak<dyn CallerConnection>>,
}

/// The caller index under [`Callers`]' one lock.
#[derive(Default)]
struct CallerIndex {
    /// Per row instance, the notification names its generation declared.
    /// They outlive the connections that called it, so a name the provider
    /// did not declare answers `Undeclared` rather than `Gone`.
    declared: HashMap<InstanceId, HashSet<String>>,
    /// Per row instance and connection id, the connection's direct writer
    /// and its unfinished calls.
    entries: HashMap<(InstanceId, String), ConnectionEntry>,
}

/// The calls a row of another language has in flight, by row instance and
/// connection id (methods contract A.2, foreign `notify`): the process's only
/// caller index. A native provider keeps its own [`Caller`] and records
/// nothing here. Entries are removed when the connection closes, when the
/// admitting Rpc load unloads and when the row's generation unloads; the
/// instance's declared names are removed with its generation. No write
/// happens under this index's lock: `notify` looks up and clones the target
/// connection and lets go of the lock before it encodes or sends anything.
#[derive(Default)]
pub struct Callers(Mutex<CallerIndex>);

/// One foreign call's record in [`Callers`]: its entry exists exactly while
/// its sink holds frames, and [`finish`](Self::finish) removes it after the
/// reply is written.
pub struct CallRegistration {
    callers: Arc<Callers>,
    instance: InstanceId,
    connection: String,
    call: String,
}
impl CallRegistration {
    /// The call's reply is written: a later `notify` with its `callId` is
    /// written directly, and the index no longer holds the call.
    pub fn finish(self) {
        let Self {
            callers,
            instance,
            connection,
            call,
        } = self;
        let mut index = callers.0.lock().expect("caller index");
        if let Some(entry) = index.entries.get_mut(&(instance, connection)) {
            entry.calls.remove(&call);
        }
    }
}

impl Callers {
    /// Records one foreign call of the provider generation `instance`:
    /// `connection` holds its frames until the reply, `direct` writes without
    /// that hold, and `notifications` are the names the serving generation
    /// declared. A connection that closed before this call reached the index
    /// gets no entry: the close already removed its entries and will not run
    /// again. The returned registration removes the call from the index.
    pub fn record(
        self: &Arc<Self>,
        instance: InstanceId,
        connection: &Arc<dyn CallerConnection>,
        direct: Arc<dyn CallerConnection>,
        call: &str,
        notifications: &HashSet<String>,
        closed: &CancellationToken,
    ) -> CallRegistration {
        let connection_id = connection.id().to_owned();
        let mut index = self.0.lock().expect("caller index");
        index
            .declared
            .entry(instance)
            .or_default()
            .extend(notifications.iter().cloned());
        if !closed.is_cancelled() {
            let entry = index
                .entries
                .entry((instance, connection_id.clone()))
                .or_insert_with(|| ConnectionEntry {
                    connection: direct.clone(),
                    calls: HashMap::new(),
                });
            entry.connection = direct;
            entry
                .calls
                .insert(call.to_owned(), Arc::downgrade(connection));
        }
        CallRegistration {
            callers: self.clone(),
            instance,
            connection: connection_id,
            call: call.to_owned(),
        }
    }
    /// Writes `method` for the call `call`: through that call while its reply
    /// is not written, and directly once it is or when the id is not in
    /// flight. A name the generation did not declare is `Undeclared` before
    /// the connection is looked at; a connection the generation received no
    /// call from, or one that closed, is `Gone`.
    pub fn notify(
        &self,
        instance: InstanceId,
        connection: &str,
        call: &str,
        method: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<(), NotifyError> {
        // Only look up and clone under the lock: encoding a large frame and
        // taking the sink's lock must not hold the process's index.
        let target = {
            let index = self.0.lock().expect("caller index");
            let Some(declared) = index.declared.get(&instance) else {
                return Err(NotifyError::Gone);
            };
            if !declared.contains(method) {
                return Err(NotifyError::Undeclared);
            }
            let Some(entry) = index.entries.get(&(instance, connection.to_owned())) else {
                return Err(NotifyError::Gone);
            };
            match entry.calls.get(call).and_then(Weak::upgrade) {
                Some(sink) => sink,
                None => entry.connection.clone(),
            }
        };
        target.notify(method, params)
    }
    /// The connection to await for `closed({connectionId})`; an id the
    /// generation never received a call from is `Gone`.
    pub fn closed(
        &self,
        instance: InstanceId,
        connection: &str,
    ) -> Result<BoxFuture<'static, ()>, NotifyError> {
        let index = self.0.lock().expect("caller index");
        let entry = index
            .entries
            .get(&(instance, connection.to_owned()))
            .ok_or(NotifyError::Gone)?;
        Ok(entry.connection.closed())
    }
    /// The admitting Rpc load unloaded: no call it admitted can be named.
    pub fn clear(&self) {
        *self.0.lock().expect("caller index") = CallerIndex::default();
    }
    /// `connection` closed: every generation's entries for it are gone. The
    /// declared names stay until their generation unloads.
    pub fn close_connection(&self, connection: &str) {
        self.0
            .lock()
            .expect("caller index")
            .entries
            .retain(|(_, id), _| id != connection);
    }
    /// The row generation `instance` unloaded: its entries and declared names
    /// are gone.
    pub fn forget_instance(&self, instance: InstanceId) {
        let mut index = self.0.lock().expect("caller index");
        index.declared.remove(&instance);
        index.entries.retain(|(owner, _), _| *owner != instance);
    }
    #[cfg(test)]
    fn count(&self) -> usize {
        self.0.lock().expect("caller index").entries.len()
    }
}

/// The progress channel of one call (methods contract A.2, `progress`): the
/// provider sends progress data on the call it serves, and the caller's entry
/// delivers it to the calling connection only, before the call's reply. Data
/// sent on a method that does not declare `progress`, or once the reply has
/// been handed over, is dropped. Delivery is best effort: the entry may also
/// drop progress its connection cannot take, which never affects the reply.
#[derive(Clone)]
pub struct Progress(Arc<Mutex<Option<Sink>>>);
impl Progress {
    pub fn send(&self, data: Value) {
        if let Some(sink) = &*self.0.lock().unwrap() {
            sink(data);
        }
    }
    /// Ends the channel. A send in flight completes first, so everything the
    /// caller received precedes the reply handed over next.
    fn close(&self) {
        self.0.lock().unwrap().take();
    }
}

/// How the provider replies to an admitted call that an unload interrupts:
/// `Report` replies `operation_interrupted` at once and settles afterwards;
/// `Drain` replies with the call's own result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interruption {
    Report,
    Drain,
}

/// One declared method. Serialized as the listing entry `{name, mutating}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodDecl {
    /// Full wire name inside the provider's namespace.
    pub name: String,
    pub mutating: bool,
    #[serde(skip_serializing)]
    pub interruption: Interruption,
    /// Whether the caller receives the progress the provider sends.
    #[serde(skip_serializing)]
    pub progress: bool,
    /// The notification names inside the namespace the provider may send on
    /// this method's caller (A.2). Checked once when the generation is built.
    #[serde(default, skip_serializing)]
    pub notifications: Vec<String>,
}
impl MethodDecl {
    /// A method without progress or notifications.
    pub fn new(name: &str, mutating: bool, interruption: Interruption) -> Self {
        Self {
            name: name.into(),
            mutating,
            interruption,
            progress: false,
            notifications: Vec::new(),
        }
    }
}

/// The methods one provider generation serves under its namespace, and the
/// calls it admitted. The generation owns them: its unload interrupts
/// `Report` calls and waits for every call to settle, so `Drain` calls hold
/// the unload.
pub struct MethodProvider {
    declarations: Vec<MethodDecl>,
    handler: Handler,
    /// The notification names of the whole declaration set: a notification
    /// belongs to the generation, not to one of its methods, so it is
    /// declared once.
    notifications: Arc<HashSet<String>>,
    /// The provider generation's own fiber, which ties it to the row that
    /// registered it.
    instance: InstanceId,
    /// Whether the handler belongs to a part of another language: only those
    /// record their calls in [`Callers`].
    foreign: bool,
    /// The caller index this generation first recorded into, for the unload
    /// effect that removes the instance's entries.
    index: Mutex<Option<Arc<Callers>>>,
    gate: Mutex<()>,
    stopping: CancellationToken,
    calls: TaskTracker,
}

impl MethodProvider {
    /// Rejects an invalid namespace or declarations (A.1: grammar, at least
    /// one method, each unique and inside the namespace; and its declared
    /// notifications: grammar, inside the namespace, unique across the set
    /// and never a method's name), then registers the unload effect on
    /// `ctx`, which waits for every admitted call.
    ///
    /// The generation of `ctx` stops admitting and interrupts its calls when
    /// it is cancelled, the first step of its unload, before any of its
    /// effects runs. Effects run last in, first out, and a row of another
    /// language unloads its plugin there in the effect its apply returns,
    /// which runs before the effects registered during that apply; so its
    /// accepted calls are told to settle while the plugin still runs.
    pub fn new(
        ctx: &Ctx,
        namespace: &str,
        declarations: Vec<MethodDecl>,
        handler: Handler,
        foreign: bool,
    ) -> Result<Arc<Self>, Error> {
        let mut unique = HashSet::new();
        let mut notifications = HashSet::new();
        if !valid_name(namespace)
            || declarations.is_empty()
            || declarations.iter().any(|decl| {
                !valid_name(&decl.name)
                    || !within(&decl.name, namespace)
                    || !unique.insert(&decl.name)
            })
            || declarations.iter().any(|decl| {
                decl.notifications.iter().any(|name| {
                    !valid_name(name)
                        || !within(name, namespace)
                        || unique.contains(name)
                        || !notifications.insert(name.clone())
                })
            })
        {
            return Err(rutis::Error::from(ErrorKind::PluginFailed(
                format!("invalid methods for namespace {namespace}").into(),
            )));
        }
        let provider = Arc::new(Self {
            declarations,
            handler,
            notifications: Arc::new(notifications),
            instance: ctx.instance(),
            foreign,
            index: Mutex::new(None),
            gate: Mutex::new(()),
            stopping: ctx.cancellation_token().child_token(),
            calls: TaskTracker::new(),
        });
        let owner = provider.clone();
        ctx.effect(move || async move {
            {
                let _guard = owner.gate.lock().unwrap();
                owner.stopping.cancel();
                owner.calls.close();
            }
            owner.calls.wait().await;
            if let Some(index) = owner.index.lock().unwrap().take() {
                index.forget_instance(owner.instance);
            }
            Ok(())
        })?;
        Ok(provider)
    }

    pub fn declarations(&self) -> &[MethodDecl] {
        &self.declarations
    }

    /// Admits one call of this generation; a name it does not declare is
    /// `unknown_method`. Both this provider's unload and the calling load's
    /// unload (`entry_stopping`) interrupt a `Report` call, which replies at
    /// once and then settles; a `Drain` call replies with its own result. The
    /// handler's token fires on either unload. A caller's disconnect cancels
    /// nothing. `progress` delivers the call's progress to its caller when
    /// the method declares it; the channel closes before the reply is handed
    /// over. `site` is the client connection the call arrived on: the handler
    /// receives its [`Caller`] bound to this generation's declared
    /// notifications; once this generation unloads, the caller answers
    /// `Gone`, like the foreign binding's index entry. A provider of another
    /// language also records the call
    /// under its `callId`, so a notification follows this call's reply until
    /// [`Accepted::registration`] ends it.
    pub fn accept(
        &self,
        method: String,
        params: Value,
        site: Option<CallSite>,
        entry_stopping: CancellationToken,
        progress: impl Fn(Value) + Send + Sync + 'static,
    ) -> Result<Accepted, RpcError> {
        let decl = self
            .declarations
            .iter()
            .find(|decl| decl.name == method)
            .ok_or_else(RpcError::unknown_method)?;
        let interruption = decl.interruption;
        let progress = Progress(Arc::new(Mutex::new(
            decl.progress.then(|| Box::new(progress) as Sink),
        )));
        let _guard = self.gate.lock().unwrap();
        if self.stopping.is_cancelled() {
            return Err(RpcError::reloading());
        }
        let mut registration = None;
        let caller = site.map(|site| {
            let call = self.foreign.then(|| uuid::Uuid::new_v4().to_string());
            let caller = Caller(Arc::new(CallerInner {
                connection: site.connection.clone(),
                notifications: self.notifications.clone(),
                stopping: self.stopping.clone(),
                call: call.clone(),
            }));
            if let Some(call) = call {
                *self.index.lock().unwrap() = Some(site.callers.clone());
                registration = Some(site.callers.record(
                    self.instance,
                    &site.connection,
                    site.direct,
                    &call,
                    &self.notifications,
                    &site.closed,
                ));
            }
            caller
        });
        let call = MethodCall {
            method,
            params,
            origin: Origin { caller },
        };
        let (send, reply) = tokio::sync::oneshot::channel();
        let (settle, settled) = tokio::sync::oneshot::channel::<()>();
        let (handler, stopping, cancel) = (
            self.handler.clone(),
            self.stopping.clone(),
            CancellationToken::new(),
        );
        self.calls.spawn(async move {
            let _settle = settle;
            let operation = handler(call, cancel.clone(), progress.clone());
            tokio::pin!(operation);
            let unloading = async {
                tokio::select! {
                    _ = entry_stopping.cancelled() => {},
                    _ = stopping.cancelled() => {},
                }
            };
            let result = tokio::select! {
                biased;
                _ = unloading => None,
                result = &mut operation => Some(result),
            };
            let result = match result {
                Some(result) => result,
                None => {
                    cancel.cancel();
                    if interruption == Interruption::Report {
                        progress.close();
                        let _ = send.send(Err(RpcError::interrupted()));
                        let _ = operation.await;
                        return;
                    }
                    operation.await
                }
            };
            progress.close();
            let _ = send.send(result);
        });
        Ok(Accepted {
            reply,
            settled,
            registration,
        })
    }
}

/// An admitted call: its reply, and a signal that closes once its operation
/// has settled, which may follow an interrupted reply.
pub struct Accepted {
    pub reply: tokio::sync::oneshot::Receiver<Result<Value, RpcError>>,
    pub settled: tokio::sync::oneshot::Receiver<()>,
    /// The caller-index entry a foreign call holds until its reply is
    /// written; `None` for a native provider.
    pub registration: Option<CallRegistration>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum DirectReply {
    Value,
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum ForeignReply {
    Value {
        kind: DirectReply,
        #[serde(default)]
        value: Value,
    },
    /// Error data; Rpc maps it to the wire (plugin contract R3).
    Error { code: String, message: String },
}
/// The error a contributed method rethrew, as Rpc maps it: a native
/// `AgentError` as Rpc maps that error, another error with a `code` as that
/// service error. `None` for a throw without a code: a binding failure.
fn rethrown(error: &rutis_bridge::session::Error) -> Option<RpcError> {
    let rutis_bridge::session::Error::Remote { name, message, .. } = error else {
        return None;
    };
    let code = error.code()?;
    Some(if name == "AgentError" {
        RpcError::agent(code, message.clone())
    } else {
        RpcError::service(code, message.clone())
    })
}

/// The handler of a namespace a part of another language registered: `call`
/// is the part's function, called with the call, a Promise that settles
/// when the call is cancelled, and a progress callback.
///
/// The Promise settles before the row's plugin is unloaded in its runtime
/// ([`MethodProvider::new`]), so a call that settles on it replies with its
/// own result. One that is still running when the plugin unloads cannot
/// reply: it is `operation_interrupted`, and it may already have taken
/// effect (rutis usage R6).
pub fn foreign_handler(call: Reference) -> Handler {
    Arc::new(
        move |MethodCall {
                  method,
                  params,
                  origin,
              }: MethodCall,
              cancel,
              progress| {
            let call = call.clone();
            Box::pin(async move {
                let interrupted = cancel.clone();
                let _finish = cancel.clone().drop_guard();
                let cancelled = BridgeValue::future(async move {
                    cancel.cancelled().await;
                    Ok(BridgeValue::Undefined)
                });
                let progress = BridgeValue::callback(move |args| {
                    let [data]: [BridgeValue; 1] = args
                        .list()?
                        .try_into()
                        .map_err(|_| BridgeError::Value("progress needs one argument".into()))?;
                    progress.send(data.json()?);
                    Ok(BridgeValue::Undefined)
                });
                let call_id = origin
                    .caller
                    .as_ref()
                    .and_then(Caller::call_id)
                    .map(str::to_owned)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let mut request = json!({"callId": call_id, "method": method, "params": params});
                if let Some(caller) = &origin.caller {
                    request["origin"] = json!({ "connectionId": caller.id() });
                }
                let reply = async {
                    let value = call
                        .call_async(BridgeValue::List(vec![request.into(), cancelled, progress]))
                        .await?;
                    settle(value).await?.json()
                }
                .await;
                let reply = reply
                    .and_then(|value| {
                        serde_json::from_value::<ForeignReply>(value)
                            .map_err(|error| BridgeError::Value(error.to_string()))
                    })
                    .map_err(|error| {
                        rethrown(&error).unwrap_or_else(|| {
                            if interrupted.is_cancelled() {
                                return RpcError::interrupted();
                            }
                            tracing::warn!(%error, "foreign method failed");
                            RpcError::wire(-32603, "Method binding failed")
                        })
                    })?;
                match reply {
                    ForeignReply::Value {
                        kind: DirectReply::Value,
                        value,
                    } => Ok(value),
                    ForeignReply::Error { code, message } => Err(RpcError::service(&code, message)),
                }
            })
        },
    )
}

/// Checks a namespace and its declarations before they are registered: the
/// names an owner keeps to itself, such as the protocol's built-in methods.
pub type Check = Arc<dyn Fn(&str, &[MethodDecl]) -> Result<(), Error> + Send + Sync>;

struct Entry {
    provider: Arc<MethodProvider>,
}

/// Who answers a method name in a table.
pub enum Owner {
    /// The namespace's current provider.
    Active(Arc<MethodProvider>),
    /// The namespace was registered and withdrawn; the row that registered it
    /// last, which tells a reloading part from a failed one.
    Left { row: Option<String> },
    /// Nothing ever registered a namespace that holds the name.
    None,
}

/// A table of method namespaces, each registered by the part that serves it
/// until that part unloads. A name belongs to the longest namespace that
/// holds it, also while that namespace is between registrations.
pub struct MethodTable {
    entries: Arc<Registrations<Entry>>,
    /// Namespaces registered once and withdrawn since, with their last row.
    left: Arc<Mutex<HashMap<String, Option<String>>>>,
    check: Check,
}

impl MethodTable {
    pub fn new(check: Check) -> Self {
        Self {
            entries: Arc::default(),
            left: Arc::default(),
            check,
        }
    }

    /// Registers `namespace` on behalf of the part that owns `ctx`, until
    /// that part unloads: its declarations are checked as A.1 requires, a
    /// namespace registered already fails, and the calls the registration
    /// admits belong to it (an unload interrupts `report` calls and waits for
    /// every call).
    pub fn register(
        &self,
        ctx: &Ctx,
        namespace: &str,
        declarations: Vec<MethodDecl>,
        handler: Handler,
    ) -> Result<(), Error> {
        self.register_with(ctx, namespace, declarations, handler, false)
    }

    fn register_with(
        &self,
        ctx: &Ctx,
        namespace: &str,
        declarations: Vec<MethodDecl>,
        handler: Handler,
        foreign: bool,
    ) -> Result<(), Error> {
        (self.check)(namespace, &declarations)?;
        let row = ctx.row();
        let provider = MethodProvider::new(ctx, namespace, declarations, handler, foreign)?;
        self.entries.register(ctx, namespace, Entry { provider })?;
        self.left.lock().unwrap().remove(namespace);
        let (left, namespace) = (Arc::downgrade(&self.left), namespace.to_owned());
        ctx.effect(move || async move {
            if let Some(left) = left.upgrade() {
                left.lock().unwrap().insert(namespace, row);
            }
            Ok(())
        })?;
        Ok(())
    }

    /// The owner of `name`: the longest registered or withdrawn namespace
    /// that holds it. A name outside the declaration grammar has none.
    pub fn owner(&self, name: &str) -> Owner {
        if !valid_name(name) {
            return Owner::None;
        }
        let registered = self
            .entries
            .all()
            .into_iter()
            .filter(|entry| within(name, &entry.name))
            .max_by_key(|entry| entry.name.len());
        let left = self
            .left
            .lock()
            .unwrap()
            .iter()
            .filter(|(namespace, _)| within(name, namespace))
            .max_by_key(|(namespace, _)| namespace.len())
            .map(|(namespace, row)| (namespace.clone(), row.clone()));
        match (registered, left) {
            (Some(entry), Some((namespace, _))) if namespace.len() <= entry.name.len() => {
                Owner::Active(entry.value.provider.clone())
            }
            (Some(entry), None) => Owner::Active(entry.value.provider.clone()),
            (_, Some((_, row))) => Owner::Left { row },
            (None, None) => Owner::None,
        }
    }

    /// The provider of `name` registered now, if any.
    pub fn provider(&self, name: &str) -> Option<Arc<MethodProvider>> {
        match self.owner(name) {
            Owner::Active(provider) => Some(provider),
            Owner::Left { .. } | Owner::None => None,
        }
    }

    /// Whether a registered or withdrawn namespace holds `name`.
    pub fn holds(&self, name: &str) -> bool {
        !matches!(self.owner(name), Owner::None)
    }

    /// The methods of the registered namespaces, each under the namespace
    /// that owns it, by name.
    pub fn listed(&self) -> Vec<MethodDecl> {
        let entries = self.entries.all();
        let mut methods: Vec<MethodDecl> = entries
            .iter()
            .flat_map(|entry| {
                entry
                    .value
                    .provider
                    .declarations()
                    .iter()
                    .filter(|decl| {
                        entries
                            .iter()
                            .filter(|other| within(&decl.name, &other.name))
                            .max_by_key(|other| other.name.len())
                            .is_some_and(|owner| owner.name == entry.name)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect();
        methods.sort_by(|a, b| a.name.cmp(&b.name));
        methods
    }

    /// Registers a namespace for a part of another language: `declarations`
    /// as data and `call` the part's function.
    fn register_foreign(
        &self,
        ctx: &Ctx,
        namespace: &str,
        declarations: serde_json::Value,
        call: Reference,
    ) -> Result<(), Error> {
        let declarations: Vec<MethodDecl> = serde_json::from_value(declarations)
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
        self.register_with(ctx, namespace, declarations, foreign_handler(call), true)
    }
}

/// `register(namespace, declarations, call)` for other languages: the
/// registration belongs to the calling row.
pub struct ForeignMethods(pub Arc<MethodTable>);
impl HostDispatch for ForeignMethods {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({ "register": "async" }))
    }
    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        if method != "register" {
            return Err(BridgeError::Value(format!("no method {method}")));
        }
        let caller = rutis_bridge::session::caller_row()
            .map(|row| row.ctx().clone())
            .ok_or_else(|| {
                BridgeError::Value(
                    "register needs the calling row's context: only a row of a language runtime can call it"
                        .into(),
                )
            })?;
        let mut args = match args {
            BridgeValue::Undefined => Vec::new(),
            args => args.list()?,
        };
        if args.len() > 3 {
            return Err(BridgeError::Value(format!(
                "expected at most 3 arguments, received {}",
                args.len()
            )));
        }
        args.resize(3, BridgeValue::Undefined);
        let mut args = args.into_iter();
        let namespace: String = rutis_bridge::session::decode_value(args.next().expect("three"))?;
        let declarations = args.next().expect("three").json()?;
        let Some(BridgeValue::Reference(call)) = args.next().filter(
            |call| matches!(call, BridgeValue::Reference(reference) if reference.is_function()),
        ) else {
            return Err(BridgeError::Value(
                "register needs a namespace, its declarations and its call function".into(),
            ));
        };
        let registered = self
            .0
            .register_foreign(&caller, &namespace, declarations, call)
            .map(|()| BridgeValue::Undefined)
            .map_err(|error| BridgeError::Value(error.to_string()));
        Ok(BridgeValue::future(async move { registered }))
    }
}

/// One declared method as the reservation question reads it: the name, whether
/// it mutates and the notification names it may send. `interruption` and
/// `progress` belong to an actual registration, not to the question, so they do
/// not cross.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReservedMethod {
    pub name: String,
    pub mutating: bool,
    #[serde(default)]
    pub notifications: Vec<String>,
}

/// A namespace and the declarations it would register.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReservedMethodsRequest {
    pub namespace: String,
    #[serde(default)]
    pub declarations: Vec<ReservedMethod>,
}

/// What `reservedMethods` fails with: `{code, message}`.
#[derive(Debug, Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReservedMethodsError {
    #[error("{message}")]
    Reserved { message: String },
}

/// The method names a host keeps from Session namespaces, such as its
/// protocol's built-ins and roots: a process value the Session's table
/// consults when a namespace is registered. A host that serves no protocol
/// names no such value, and its Session tables reserve nothing.
pub struct ReservedMethods(Check);

impl ReservedMethods {
    /// Binds the owner's check as the face's operation.
    pub fn new(check: Check) -> Self {
        Self(check)
    }

    /// The check a [`MethodTable`] consults for the Session's own
    /// registrations.
    pub(crate) fn check_type(&self) -> Check {
        self.0.clone()
    }
}

#[rutis::service]
impl ReservedMethods {
    /// Refuses a namespace and declarations the host keeps for itself.
    pub fn check(
        &self,
        request: ReservedMethodsRequest,
    ) -> std::result::Result<(), ReservedMethodsError> {
        let declarations: Vec<MethodDecl> = request
            .declarations
            .into_iter()
            .map(|declaration| {
                let mut method =
                    MethodDecl::new(&declaration.name, declaration.mutating, Interruption::Drain);
                method.notifications = declaration.notifications;
                method
            })
            .collect();
        (self.0)(&request.namespace, &declarations).map_err(|error| {
            ReservedMethodsError::Reserved {
                message: error.to_string(),
            }
        })
    }
}

/// The Session's method table: the namespaces its parts register, which Rpc
/// dispatches through the Session (methods contract A.1).
pub struct SessionMethods(pub Arc<MethodTable>);
impl std::ops::Deref for SessionMethods {
    type Target = MethodTable;
    fn deref(&self) -> &MethodTable {
        &self.0
    }
}

/// Session part: the Session's method table, at its native key and, for
/// other languages, at its dispatcher key.
pub struct SessionMethodsPlugin {
    session: InstanceId,
    table: Key<SessionMethods>,
    reserved: Option<Key<ReservedMethods>>,
}
impl SessionMethodsPlugin {
    /// The table of this copy's Session. When the assembly names
    /// [`ReservedMethods`] (App Server does, with the names its protocol
    /// keeps), the table waits for them and refuses what they reserve.
    pub fn part(names: &Names<'_>) -> Result<Box<dyn DynPlugin>, Error> {
        Ok(Box::new(Self {
            session: names.instance(dim_agent_runtime::SESSION_GROUP)?,
            table: names.key()?,
            reserved: names
                .registered::<ReservedMethods>()
                .then(|| names.key::<ReservedMethods>())
                .transpose()?,
        }))
    }
}
impl DynPlugin for SessionMethodsPlugin {
    fn name(&self) -> &str {
        "SessionMethods"
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.reserved.iter().map(Key::erase).collect()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let check: Check = match &self.reserved {
                Some(reserved) => ctx.require_at(reserved)?.check_type(),
                None => Arc::new(|_, _| Ok(())),
            };
            let table = Arc::new(MethodTable::new(check));
            ctx.provide_at::<dyn HostDispatch>(
                host_key_in(
                    &dim_agent_runtime::names::name_of::<SessionMethods>(),
                    self.session,
                ),
                Arc::new(ForeignMethods(table.clone())),
            )?;
            ctx.provide_at(self.table.clone(), Arc::new(SessionMethods(table)))?;
            Ok(Effect::Done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection stand-in that records which handle wrote a notification.
    struct Channel {
        id: &'static str,
        tag: &'static str,
        writes: Arc<Mutex<Vec<String>>>,
    }
    impl CallerConnection for Channel {
        fn id(&self) -> &str {
            self.id
        }
        fn notify(
            &self,
            method: &str,
            _: serde_json::Map<String, Value>,
        ) -> Result<(), NotifyError> {
            self.writes
                .lock()
                .unwrap()
                .push(format!("{}:{method}", self.tag));
            Ok(())
        }
        fn closed(&self) -> BoxFuture<'static, ()> {
            Box::pin(std::future::pending())
        }
    }
    fn channel(
        id: &'static str,
        tag: &'static str,
        writes: &Arc<Mutex<Vec<String>>>,
    ) -> Arc<dyn CallerConnection> {
        Arc::new(Channel {
            id,
            tag,
            writes: writes.clone(),
        })
    }
    fn names() -> HashSet<String> {
        HashSet::from(["notes.changed".to_owned()])
    }

    /// F2: the index holds a connection entry only until the connection
    /// closes or the row unloads, and a call entry only until its reply is
    /// written. A finished call's id writes directly.
    #[tokio::test]
    async fn the_index_forgets_finished_calls_closed_connections_and_unloaded_rows() {
        let first = Ctx::root().unwrap();
        let second = Ctx::root().unwrap();
        let callers = Arc::new(Callers::default());
        let writes = Arc::new(Mutex::new(Vec::new()));
        let sink = channel("first", "held", &writes);
        let registration = callers.record(
            first.instance(),
            &sink,
            channel("first", "direct", &writes),
            "call-1",
            &names(),
            &CancellationToken::new(),
        );
        assert_eq!(callers.count(), 1);
        callers
            .notify(
                first.instance(),
                "first",
                "call-1",
                "notes.changed",
                Default::default(),
            )
            .unwrap();
        callers
            .notify(
                first.instance(),
                "first",
                "finished",
                "notes.changed",
                Default::default(),
            )
            .unwrap();
        assert_eq!(
            *writes.lock().unwrap(),
            ["held:notes.changed", "direct:notes.changed"]
        );
        assert_eq!(
            callers.notify(
                first.instance(),
                "absent",
                "call-1",
                "notes.changed",
                Default::default()
            ),
            Err(NotifyError::Gone)
        );
        assert_eq!(
            callers.notify(
                first.instance(),
                "first",
                "call-1",
                "notes.other",
                Default::default()
            ),
            Err(NotifyError::Undeclared)
        );
        // The reply is written: the call entry is gone and later ids write
        // directly.
        registration.finish();
        callers
            .notify(
                first.instance(),
                "first",
                "call-1",
                "notes.changed",
                Default::default(),
            )
            .unwrap();
        assert_eq!(
            writes.lock().unwrap().last().unwrap(),
            "direct:notes.changed"
        );
        // Closing one connection leaves another row's entries; unloading the
        // row removes its own.
        let other = channel("second", "held", &writes);
        callers.record(
            second.instance(),
            &other,
            channel("second", "direct", &writes),
            "call-2",
            &names(),
            &CancellationToken::new(),
        );
        assert_eq!(callers.count(), 2);
        callers.close_connection("first");
        assert_eq!(callers.count(), 1);
        // A closed connection answers a name the generation declared `Gone`,
        // and an undeclared name `Undeclared`: the provider's error comes
        // first in both bindings (A.2).
        assert_eq!(
            callers.notify(
                first.instance(),
                "first",
                "call-1",
                "notes.changed",
                Default::default()
            ),
            Err(NotifyError::Gone)
        );
        assert_eq!(
            callers.notify(
                first.instance(),
                "first",
                "call-1",
                "notes.other",
                Default::default()
            ),
            Err(NotifyError::Undeclared)
        );
        callers.forget_instance(second.instance());
        assert_eq!(callers.count(), 0);
        // The generation that unloaded declared nothing any more: its names
        // are `Gone`, as they are for a native caller after the same unload.
        assert_eq!(
            callers.notify(
                second.instance(),
                "second",
                "call-2",
                "notes.other",
                Default::default()
            ),
            Err(NotifyError::Gone)
        );
        first.shutdown().await.unwrap();
        second.shutdown().await.unwrap();
    }

    /// F6: a call admitted as its connection closes records no entry, so the
    /// index keeps nothing for a connection that will never see `finish` run
    /// against it again.
    #[tokio::test]
    async fn a_call_recorded_after_its_connection_closed_leaves_no_entry() {
        let root = Ctx::root().unwrap();
        let callers = Arc::new(Callers::default());
        let writes = Arc::new(Mutex::new(Vec::new()));
        let closed = CancellationToken::new();
        closed.cancel();
        callers.record(
            root.instance(),
            &channel("first", "held", &writes),
            channel("first", "direct", &writes),
            "call-1",
            &names(),
            &closed,
        );
        assert_eq!(callers.count(), 0);
        assert_eq!(
            callers.notify(
                root.instance(),
                "first",
                "call-1",
                "notes.other",
                Default::default()
            ),
            Err(NotifyError::Undeclared)
        );
        assert!(writes.lock().unwrap().is_empty());
        root.shutdown().await.unwrap();
    }
}

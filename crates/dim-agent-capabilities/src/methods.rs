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
    sync::{Arc, Mutex},
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

/// Serves one call: `(method, params, cancellation token, progress)`. The
/// token fires when the provider's generation or the calling load unloads.
pub type Handler = Arc<
    dyn Fn(
            String,
            Value,
            CancellationToken,
            Progress,
        ) -> BoxFuture<'static, Result<Value, RpcError>>
        + Send
        + Sync,
>;

type Sink = Box<dyn Fn(Value) + Send + Sync>;

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
}
impl MethodDecl {
    /// A method without progress.
    pub fn new(name: &str, mutating: bool, interruption: Interruption) -> Self {
        Self {
            name: name.into(),
            mutating,
            interruption,
            progress: false,
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
    gate: Mutex<()>,
    stopping: CancellationToken,
    calls: TaskTracker,
}

impl MethodProvider {
    /// Rejects an invalid namespace or declarations (A.1: grammar, at least
    /// one method, each unique and inside the namespace), then registers the
    /// unload effect on `ctx`, which waits for every admitted call.
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
    ) -> Result<Arc<Self>, Error> {
        let mut unique = HashSet::new();
        if !valid_name(namespace)
            || declarations.is_empty()
            || declarations.iter().any(|decl| {
                !valid_name(&decl.name)
                    || !within(&decl.name, namespace)
                    || !unique.insert(&decl.name)
            })
        {
            return Err(rutis::Error::from(ErrorKind::PluginFailed(
                format!("invalid methods for namespace {namespace}").into(),
            )));
        }
        let provider = Arc::new(Self {
            declarations,
            handler,
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
    /// over.
    pub fn accept(
        &self,
        method: String,
        params: Value,
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
        let (send, reply) = tokio::sync::oneshot::channel();
        let (settle, settled) = tokio::sync::oneshot::channel::<()>();
        let (handler, stopping, cancel) = (
            self.handler.clone(),
            self.stopping.clone(),
            CancellationToken::new(),
        );
        self.calls.spawn(async move {
            let _settle = settle;
            let operation = handler(method, params, cancel.clone(), progress.clone());
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
        Ok(Accepted { reply, settled })
    }
}

/// An admitted call: its reply, and a signal that closes once its operation
/// has settled, which may follow an interrupted reply.
pub struct Accepted {
    pub reply: tokio::sync::oneshot::Receiver<Result<Value, RpcError>>,
    pub settled: tokio::sync::oneshot::Receiver<()>,
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
    Arc::new(move |method, params, cancel, progress| {
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
            let request = json!({"callId": uuid::Uuid::new_v4().to_string(), "method": method, "params": params});
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
    })
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
        (self.check)(namespace, &declarations)?;
        let row = ctx.row();
        let provider = MethodProvider::new(ctx, namespace, declarations, handler)?;
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
        self.register(ctx, namespace, declarations, foreign_handler(call))
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

/// The method names a host keeps from Session namespaces, such as its
/// protocol's built-ins and roots: a process value the Session's table
/// consults when a namespace is registered. A host that serves no protocol
/// names no such value, and its Session tables reserve nothing.
pub struct ReservedMethods(pub Check);

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
                Some(reserved) => ctx.require_at(reserved)?.0.clone(),
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

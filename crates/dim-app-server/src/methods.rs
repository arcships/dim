//! The management method table and Rpc's lookups through the method tables
//! (methods contract A.1, §3.2). A part registers a namespace with the table
//! of its scope until it unloads; Rpc reads the table on each call and
//! depends on no provider, so a provider that is reloading, Pending or Failed
//! affects only its own namespace. Accepted calls belong to their provider.
use dim_agent_capabilities::methods::{
    Accepted, CallSite, Check, ForeignMethods, MethodDecl, MethodProvider, MethodTable, Owner,
    RpcError, SessionMethods, valid_name, within,
};
use dim_agent_runtime::Session;
use rutis::host::{CtxHost, ErrorKind, FiberState, Key, Snapshot};
use rutis::{Ctx, Error, Plugin};
use rutis_bridge::session::{HostDispatch, host_key};
use rutis_loader::Loader;
use rutis_loader::report::EntryStatus;
use serde::Deserialize;
use serde_json::Value;
use std::{ops::Deref, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionScope {
    #[serde(default, deserialize_with = "crate::protocol::optional_non_null")]
    pub cwd: Option<String>,
}

/// The process's management methods: the namespaces process plugins
/// register, which Rpc serves to protected local connections.
pub struct ManagementMethods(pub Arc<MethodTable>);
impl Deref for ManagementMethods {
    type Target = MethodTable;
    fn deref(&self) -> &MethodTable {
        &self.0
    }
}

impl ManagementMethods {
    /// Management methods are about the host, never a Session. The `session`
    /// root belongs to built-ins and Session-scoped methods, so contributed
    /// methods cannot impersonate Session traffic.
    pub(crate) fn check_namespace(namespace: &str) -> Result<(), Error> {
        if !valid_name(namespace)
            || ["hello", "daemon", "agent", "approval", "session"]
                .iter()
                .any(|reserved| within(namespace, reserved))
            || namespace
                .split('.')
                .scan(String::new(), |prefix, part| {
                    if !prefix.is_empty() {
                        prefix.push('.');
                    }
                    prefix.push_str(part);
                    Some(crate::protocol::is_builtin_method(prefix))
                })
                .any(|reserved| reserved)
        {
            return Err(rutis::Error::from(ErrorKind::PluginFailed(
                "Invalid management namespace".into(),
            )));
        }
        Ok(())
    }

    fn check() -> Check {
        Arc::new(|namespace, declarations| {
            Self::check_namespace(namespace)?;
            if declarations.iter().any(|decl| {
                crate::protocol::is_builtin_method(&decl.name)
                    || decl
                        .notifications
                        .iter()
                        .any(|name| crate::protocol::is_builtin_notification(name))
            }) {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(
                    "Invalid management contribution".into(),
                )));
            }
            Ok(())
        })
    }
}

/// Process part: the management method table, at its native key and, for
/// other languages, at its dispatcher key.
pub struct ManagementMethodsPlugin {
    table: Key<ManagementMethods>,
}
impl ManagementMethodsPlugin {
    pub fn new(names: &dim_agent_runtime::Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            table: names.key()?,
        })
    }
    /// This part keyed from `catalog`, to mount it directly.
    pub fn typed(catalog: &dim_agent_runtime::names::ServiceCatalog) -> Result<Self, Error> {
        let build = Default::default();
        let plugin = Self::new(&dim_agent_runtime::Names::copy(catalog, &build))?;
        Ok(plugin)
    }
}
impl Plugin for ManagementMethodsPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "ManagementMethods"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        let table = Arc::new(MethodTable::new(ManagementMethods::check()));
        ctx.provide_at::<dyn HostDispatch>(
            host_key(&dim_agent_runtime::names::name_of::<ManagementMethods>()),
            Arc::new(ForeignMethods(table.clone())),
        )?;
        ctx.provide_at(self.table.clone(), Arc::new(ManagementMethods(table)))?;
        Ok(())
    }
}

/// The names a Session namespace may not take: the protocol's roots and
/// built-ins (methods contract A.1). A Session namespace may lie under
/// `session`, but never equals, contains or lies inside an exact built-in.
pub(crate) fn reserved_for_sessions() -> Check {
    Arc::new(|namespace, declarations| {
        let overlaps = |other: &str| within(namespace, other) || within(other, namespace);
        if !valid_name(namespace)
            || ["hello", "daemon", "agent", "approval"]
                .iter()
                .any(|root| within(namespace, root))
            || crate::protocol::builtin_methods().any(overlaps)
            || declarations.iter().any(|decl| {
                decl.notifications
                    .iter()
                    .any(|name| crate::protocol::is_builtin_notification(name))
            })
        {
            return Err(rutis::Error::from(ErrorKind::PluginFailed(
                format!("invalid Session namespace {namespace}").into(),
            )));
        }
        Ok(())
    })
}

/// Unlike `service_reloading`, a retry fails the same way until the owner is
/// fixed or restarted.
fn unavailable() -> RpcError {
    RpcError::app(
        -32000,
        "service_unavailable",
        "Service is unavailable until it is fixed or restarted",
    )
}

/// What a call answers when its namespace has no active provider, from the
/// status of the loader row that provides it (methods contract §3.2): `Ok`
/// when the row is Active, so the caller reads the provider again. Each
/// known state is named, and a new `FiberState` does not compile until it is
/// placed.
fn not_active(status: EntryStatus) -> Result<(), RpcError> {
    match status {
        EntryStatus::Disabled => Err(RpcError::unknown_method()),
        EntryStatus::Inactive => Err(RpcError::reloading()),
        EntryStatus::Running(Snapshot { state, .. }) => match state {
            FiberState::Pending | FiberState::Loading | FiberState::Unloading => {
                Err(RpcError::reloading())
            }
            FiberState::Active => Ok(()),
            FiberState::Failed | FiberState::Disposed => Err(unavailable()),
        },
        EntryStatus::Unresolved(_) | EntryStatus::Stopped => Err(unavailable()),
        _ => Err(unavailable()),
    }
}

/// The provider that serves management method `method`, read on each call.
/// A namespace that was registered and is withdrawn now answers from the
/// status of the row that registered it: `service_reloading` while it
/// starts or reloads, `service_unavailable` once it cannot run. A name no
/// namespace ever held, or a disabled or absent row, is `unknown_method`.
pub(crate) fn management_provider(
    table: &ManagementMethods,
    loader: &Loader,
    method: &str,
) -> Result<Arc<MethodProvider>, RpcError> {
    match table.owner(method) {
        Owner::Active(provider) => Ok(provider),
        Owner::Left { row } => {
            let row = row
                .and_then(|row| loader.get(&row))
                .ok_or_else(RpcError::unknown_method)?;
            not_active(row.status)?;
            match table.owner(method) {
                Owner::Active(provider) => Ok(provider),
                _ => Err(RpcError::unknown_method()),
            }
        }
        Owner::None => Err(RpcError::unknown_method()),
    }
}

/// Admits a management call through its provider (§3.2). `site` is the
/// calling connection, when a client connection made the call.
pub(crate) fn dispatch(
    table: &ManagementMethods,
    loader: &Loader,
    method: String,
    params: Value,
    site: Option<CallSite>,
    entry_stopping: CancellationToken,
    progress: impl Fn(Value) + Send + Sync + 'static,
) -> Result<Accepted, RpcError> {
    let provider = management_provider(table, loader, &method)?;
    provider.accept(method, params, site, entry_stopping, progress)
}

/// The provider that serves Session method `method` in `session`. A
/// namespace withdrawn in this Session answers from the row that registered
/// it: its copy in this Session, if it is a row of the `session` group, or a
/// part between generations, which is `service_reloading`. A name no
/// namespace of this Session ever held is `unknown_method`.
pub(crate) fn session_provider(
    loader: &Loader,
    session: &Session,
    method: &str,
) -> Result<Arc<MethodProvider>, RpcError> {
    let table = session.session_service::<SessionMethods>()?;
    match table.owner(method) {
        Owner::Active(provider) => Ok(provider),
        Owner::Left { row } => {
            let instance = session.instance()?;
            let copy = row.and_then(|row| {
                loader.entries().into_iter().find(|entry| {
                    entry.id == row
                        && entry
                            .instance
                            .as_ref()
                            .and_then(|copy| copy.chain.first())
                            .is_some_and(|(_, id)| *id == instance)
                })
            });
            if let Some(copy) = copy {
                not_active(copy.status)?;
            }
            match table.owner(method) {
                Owner::Active(provider) => Ok(provider),
                _ => Err(RpcError::reloading()),
            }
        }
        Owner::None => Err(RpcError::unknown_method()),
    }
}

/// The methods `session.methods` lists: those of the namespaces registered
/// in this Session now, except names a management namespace holds, which
/// routing sends to management even when their params name a Session.
pub(crate) fn session_listed(
    management: &ManagementMethods,
    session: &Session,
) -> Result<Vec<MethodDecl>, RpcError> {
    Ok(session
        .session_service::<SessionMethods>()?
        .listed()
        .into_iter()
        .filter(|decl| !management.holds(&decl.name))
        .collect())
}

#[cfg(test)]
mod tests;

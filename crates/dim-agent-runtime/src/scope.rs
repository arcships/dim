use crate::*;
use rutis::Ctx;
use rutis::host::{InstanceId, Key};

/// Explicit opt-in for services that are safe to obtain from a public Session handle.
///
/// A part's own dependencies do not require this marker; only the facades'
/// reads do, and they read a name in the group the catalog registers it in.
///
/// ```compile_fail
/// use dim_agent_runtime::{Session, Timeline};
/// fn invalid(session: &Session) {
///     let _ = session.service::<Timeline>("main");
/// }
/// ```
///
/// ```compile_fail
/// use dim_agent_runtime::{Session, StateCell};
/// fn invalid(session: &Session) {
///     let _ = session.service::<StateCell<String>>("main");
/// }
/// ```
///
/// ```compile_fail
/// use dim_agent_runtime::{Session, TimelineTreeControl};
/// fn invalid(session: &Session) {
///     let _ = session.session_service::<dyn TimelineTreeControl>();
/// }
/// ```
///
/// ```compile_fail
/// use dim_agent_runtime::{Branch, Dispatcher};
/// fn invalid(branch: &Branch) {
///     let _ = branch.service::<Dispatcher>();
/// }
/// ```
///
/// A branch facade reads history but cannot write arbitrary events:
///
/// ```compile_fail
/// use dim_agent_runtime::Branch;
/// fn invalid(branch: &Branch) {
///     let _ = branch.timeline();
/// }
/// ```
pub trait PublicService: Send + Sync + 'static {}

/// The loader that owns this Session's subtree. Not publicly accessible.
/// Only lock-free instance creation is exposed to Session capabilities.
pub struct SessionLoader {
    loader: rutis_loader::Loader,
    /// A running loader row of the instance new instances go in: the loader
    /// places an instance from a row's context, and a mounted part is not a
    /// row ([`crate::PartRegistry`]).
    anchor: Ctx,
}
impl SessionLoader {
    pub fn new(loader: rutis_loader::Loader, anchor: Ctx) -> Self {
        Self { loader, anchor }
    }
    /// Creates an instance of `group` where the configuration tree puts it
    /// inside this loader's instance.
    pub fn create_instance(&self, group: &str) -> rutis_loader::edit::CreateInstance<'_> {
        self.loader.create_instance(&self.anchor, group)
    }
}
/// Identity of one Session subtree, which the runtime builds keys from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SessionScope {
    pub instance: InstanceId,
}
impl SessionScope {
    pub fn key<T: ?Sized + 'static>(&self) -> Key<T> {
        Key::of().instance(self.instance)
    }
}

/// Identity of one branch subtree nested in its Session subtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BranchScope {
    pub session: SessionScope,
    pub instance: InstanceId,
}
impl BranchScope {
    pub fn key<T: ?Sized + 'static>(&self) -> Key<T> {
        Key::of().instance(self.instance)
    }
}
impl PublicService for SessionConfig {}
impl PublicService for dyn AgentLoop {}

use crate::*;
use rutis::{CordisError, Ctx, InstanceId, TypeKey};
use std::sync::Arc;

/// A service keyed to one Session subtree.
pub trait SessionService: Send + Sync + 'static {}
/// A service keyed to one branch subtree.
pub trait BranchService: Send + Sync + 'static {}

/// Explicit opt-in for services that are safe to obtain from a public Session handle.
///
/// Internal capability dependency lookup through [`SessionScope::service`] and
/// [`BranchScope::service`] does not require this marker.
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
pub struct SessionLoader(rutis_loader::Loader);
impl SessionLoader {
    pub fn new(loader: rutis_loader::Loader) -> Self {
        Self(loader)
    }
    pub fn create_instance<'a>(
        &'a self,
        ctx: &Ctx,
        group: &str,
    ) -> rutis_loader::CreateInstance<'a> {
        self.0.create_instance(ctx, group)
    }
}
impl SessionService for SessionLoader {}

/// Identity of one Session subtree; plugin factories receive it to build keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionScope {
    pub instance: InstanceId,
}
impl SessionScope {
    pub fn key<T: ?Sized + SessionService>(&self) -> TypeKey {
        TypeKey::instance::<T>(self.instance)
    }
    /// Reads a declared Session service; absence is a missing dependency.
    #[track_caller]
    pub fn service<T: ?Sized + SessionService>(&self, ctx: &Ctx) -> Result<Arc<T>> {
        lookup(ctx, self.key::<T>())
    }
    /// Key of one named Session service among several of the same type.
    pub fn named_key<T: ?Sized + SessionService>(&self, name: &str) -> TypeKey {
        TypeKey::keyed_dynamic::<T>(name).with_instance(self.instance)
    }
    /// Reads a declared named Session service; absence is a missing dependency.
    #[track_caller]
    pub fn named_service<T: ?Sized + SessionService>(
        &self,
        ctx: &Ctx,
        name: &str,
    ) -> Result<Arc<T>> {
        lookup(ctx, self.named_key::<T>(name))
    }
}

/// Identity of one branch subtree nested in its Session subtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BranchScope {
    pub session: SessionScope,
    pub instance: InstanceId,
}
impl BranchScope {
    pub fn key<T: ?Sized + BranchService>(&self) -> TypeKey {
        TypeKey::instance::<T>(self.instance)
    }
    /// Reads a declared branch service; absence is a missing dependency.
    #[track_caller]
    pub fn service<T: ?Sized + BranchService>(&self, ctx: &Ctx) -> Result<Arc<T>> {
        lookup(ctx, self.key::<T>())
    }
}

/// Strict read: the caller's `injects()` must declare the key.
#[track_caller]
fn lookup<T: ?Sized + Send + Sync + 'static>(ctx: &Ctx, key: TypeKey) -> Result<Arc<T>> {
    Ok(ctx.require_as::<T>(key).map_err(CordisError::from)?)
}

impl SessionService for SessionConfig {}
impl PublicService for SessionConfig {}
impl SessionService for AgentConfig {}
impl SessionService for SessionIdentity {}
impl SessionService for SessionLifecycle {}
impl SessionService for FaultReporter {}
impl SessionService for TimelineFactory {}
impl SessionService for dyn TimelineStore {}
impl SessionService for dyn TimelineTreeControl {}

impl BranchService for Timeline {}
impl BranchService for Dispatcher {}
impl BranchService for TimelineIdentity {}
impl BranchService for BranchLifecycle {}
impl<S: Clone + Send + Sync + 'static> BranchService for StateCell<S> {}
impl BranchService for dyn LoopState {}
impl PublicService for dyn LoopState {}
impl BranchService for dyn ConfigureControl {}

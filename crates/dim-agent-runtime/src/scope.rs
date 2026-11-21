use crate::*;
use rutis::Ctx;
use rutis::host::{CtxHost, InstanceId, Key};

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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionInstanceDefinition {
    pub group: String,
    pub definition_id: String,
    pub instance_data: serde_json::Value,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionInstance {
    pub instance_id: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionInstanceError {
    #[error("{0}")]
    Create(String),
    #[error("{0}")]
    NotReady(String),
    #[error("{0}")]
    Cleanup(String),
}

/// The loader anchored in this Session's subtree.
pub struct SessionLoader {
    loader: rutis_loader::Loader,
    anchor: Ctx,
    parent_group: Option<&'static str>,
    extension_groups: std::sync::Arc<[String]>,
    instances: std::sync::Mutex<std::collections::HashMap<u64, rutis::host::FiberView>>,
}
#[rutis::service]
impl SessionLoader {
    pub fn new(
        loader: rutis_loader::Loader,
        anchor: Ctx,
        parent_group: Option<&'static str>,
        extension_groups: std::sync::Arc<[String]>,
    ) -> Self {
        Self {
            loader,
            anchor,
            parent_group,
            extension_groups,
            instances: Default::default(),
        }
    }

    pub async fn create(
        &self,
        definition: SessionInstanceDefinition,
    ) -> std::result::Result<SessionInstance, SessionInstanceError> {
        let group = definition.group.clone();
        if self
            .loader
            .get(&group)
            .is_some_and(|row| row.parent.as_deref() != self.parent_group)
        {
            return Err(SessionInstanceError::Create(format!(
                "instance group {group} is outside the loader scope"
            )));
        }
        let created = self
            .loader
            .create_instance(&self.anchor, &group)
            .with(self.anchor.instance())
            .with(definition)
            .await
            .map_err(|error| SessionInstanceError::Create(error.to_string()))?;
        let instance = SessionInstance {
            instance_id: created.plugin.0,
        };
        self.instances
            .lock()
            .unwrap()
            .insert(instance.instance_id, created.view);
        if self.extension_groups.contains(&group)
            && let Some((row, state)) = created
                .report
                .iter()
                .find(|(_, state)| !matches!(state, rutis_loader::edit::InstanceResult::Active))
        {
            self.remove(instance).await?;
            return Err(SessionInstanceError::NotReady(format!("{row}: {state:?}")));
        }
        Ok(instance)
    }

    pub async fn remove(
        &self,
        instance: SessionInstance,
    ) -> std::result::Result<(), SessionInstanceError> {
        let view = self
            .instances
            .lock()
            .unwrap()
            .get(&instance.instance_id)
            .cloned();
        if let Some(view) = view {
            let result = view.shutdown().await;
            self.instances.lock().unwrap().remove(&instance.instance_id);
            result.map_err(|error| SessionInstanceError::Cleanup(error.to_string()))?;
            if let Some(error) = view.state().error {
                return Err(SessionInstanceError::Cleanup(error.to_string()));
            }
        }
        Ok(())
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
impl PublicService for dyn LoopState {}

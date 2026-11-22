//! Immutable content services owned by real Session loader instances.
use super::{CatalogView, SkillDefinition, SkillStatus, SkillView, SkillsError};
use dim_agent_runtime::{
    Names, SESSION_GROUP, SessionInstance, SessionInstanceDefinition, SessionLoader,
};
use rutis::host::{CtxHost, InstanceId, Key};
use rutis::{Ctx, Plugin};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub(super) const GROUP: &str = "SkillContentInstances";

#[derive(Debug, thiserror::Error)]
pub(super) enum ContentError {
    #[error(transparent)]
    Catalog(#[from] SkillsError),
    #[error("Skill content lifecycle failed: {0}")]
    Lifecycle(String),
}

struct Contribution(Arc<SkillDefinition>);

pub(super) struct ContentPlugin {
    definition: Arc<SkillDefinition>,
    key: Key<Contribution>,
}
impl ContentPlugin {
    pub(super) fn new(names: &Names<'_>, value: &SessionInstanceDefinition) -> rutis::Result<Self> {
        Ok(Self {
            definition: Arc::new(serde_json::from_value(value.instance_data.clone())?),
            key: Key::dynamic(value.definition_id.clone()).instance(names.instance(SESSION_GROUP)?),
        })
    }
}
impl Plugin for ContentPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "SkillContent"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide_at(
            self.key.clone(),
            Arc::new(Contribution(self.definition.clone())),
        )?;
        Ok(())
    }
}

pub(super) struct Contents {
    ctx: Ctx,
    session: InstanceId,
    loader: Arc<SessionLoader>,
    instances: BTreeMap<String, (SessionInstance, Key<Contribution>)>,
}
impl Contents {
    pub(super) fn new(ctx: Ctx, session: InstanceId, loader: Arc<SessionLoader>) -> Self {
        Self {
            ctx,
            session,
            loader,
            instances: BTreeMap::new(),
        }
    }
    pub(super) async fn close(&mut self) -> Result<(), ContentError> {
        let mut error = None;
        for (_, (instance, _)) in std::mem::take(&mut self.instances) {
            if let Err(failure) = self.loader.remove(instance).await {
                error = Some(ContentError::Lifecycle(failure.to_string()));
            }
        }
        error.map_or(Ok(()), Err)
    }
    pub(super) async fn update(
        &mut self,
        view: Result<CatalogView, SkillsError>,
        cancel: &CancellationToken,
    ) -> Result<Arc<SkillView>, SkillsError> {
        let mut result = self.reconcile(view, cancel).await;
        if result.is_err()
            && let Err(error) = self.close().await
        {
            result = Err(error);
        }
        result.map_err(|error| match error {
            ContentError::Catalog(error) => error,
            ContentError::Lifecycle(error) => {
                tracing::error!(%error, "Skill contents unavailable");
                SkillsError::Closed
            }
        })
    }
    async fn reconcile(
        &mut self,
        view: Result<CatalogView, SkillsError>,
        cancel: &CancellationToken,
    ) -> Result<Arc<SkillView>, ContentError> {
        let view = view?;
        let selected: BTreeMap<_, _> = view
            .entries
            .iter()
            .filter(|entry| entry.status == SkillStatus::Active && entry.enabled)
            .map(|entry| (entry.definition.id.clone(), entry.definition.clone()))
            .collect();
        let disabled: BTreeSet<_> = view
            .entries
            .iter()
            .filter(|entry| entry.status == SkillStatus::Active && !entry.enabled)
            .map(|entry| entry.definition.name.clone())
            .collect();
        let obsolete: Vec<_> = {
            self.instances
                .iter()
                .filter(|(id, (_, key))| {
                    selected.get(*id).is_none_or(|definition| {
                        self.ctx
                            .get_at(key)
                            .is_none_or(|service| **definition != *service.0)
                    })
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in obsolete {
            let (instance, _) = self.instances.remove(&id).unwrap();
            self.loader
                .remove(instance)
                .await
                .map_err(|error| ContentError::Lifecycle(error.to_string()))?;
        }
        for (id, definition) in selected {
            if cancel.is_cancelled() {
                return Err(SkillsError::Closed.into());
            }
            if self.instances.contains_key(&id) {
                continue;
            }
            let provided = Key::<Contribution>::dynamic(id.clone()).instance(self.session);
            let instance = self
                .loader
                .create(SessionInstanceDefinition {
                    group: GROUP.to_owned(),
                    definition_id: id.clone(),
                    instance_data: serde_json::json!(definition),
                })
                .await
                .map_err(|error| ContentError::Lifecycle(error.to_string()))?;
            if cancel.is_cancelled() || self.ctx.get_at(&provided).is_none() {
                self.loader
                    .remove(instance)
                    .await
                    .map_err(|error| ContentError::Lifecycle(error.to_string()))?;
                return Err(if cancel.is_cancelled() {
                    SkillsError::Closed.into()
                } else {
                    ContentError::Lifecycle("content instance did not become ready".into())
                });
            }
            self.instances.insert(id, (instance, provided));
        }
        Ok(Arc::new(SkillView::from_contributions(
            self.instances
                .values()
                .filter_map(|(_, key)| self.ctx.get_at(key))
                .map(|service| (service.0.name.clone(), service.0.clone()))
                .collect(),
            disabled,
        )))
    }
}

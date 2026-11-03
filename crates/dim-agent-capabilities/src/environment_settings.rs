//! Model-facing execution settings: the sandbox and network switches as state.
//!
//! The App Server's `ExecutionSettings` owns the Settings document and
//! provides `ExecutionSettingsHost`. This capability registers
//! `dim/settings/execution` and adopts it when the source changes or when the
//! operator's input arrives; an assembly without the host registers nothing.
use crate::{
    execution_settings::{ExecutionSettings, ExecutionSettingsHost},
    host::{HostError, HostFuture, LoadPrompt},
    prompt_registry::{PromptRegistry, targets},
    prompt_sources::sha256_version,
    settings::{SettingsChanged, settings_changes},
};
use dim_agent_runtime::{
    AgentBuilder, ConfigureControl, ConfigureUpdate, Delivery, Dispatcher, Feature, Names,
    PromptSourceTarget,
};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The prompt source of the execution switches.
pub const SOURCE: &str = "dim/settings/execution";
/// The Settings source whose committed writes wake the projection.
const SETTINGS_SOURCE: &str = "execution";

/// The body: effective values and when they apply.
fn body(settings: ExecutionSettings) -> String {
    if settings.sandbox {
        format!(
            "Execution sandbox: on; network: {}. New processes only; a running shell keeps its original boundary.",
            if settings.network { "on" } else { "off" }
        )
    } else {
        "Execution sandbox: off. New processes run with the host's normal access.".into()
    }
}

struct SettingsSource {
    host: Arc<ExecutionSettingsHost>,
}
impl LoadPrompt for SettingsSource {
    fn load(
        &self,
        target: PromptSourceTarget,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled(
                    "execution settings load cancelled".into(),
                ));
            }
            let settings = self
                .host
                .0
                .read(cancel.child_token())
                .await
                .map_err(|error| HostError::failed(error.to_string()))?;
            let body = body(settings);
            Ok(Some(PromptEntry {
                source: target.source,
                version: sha256_version(&body),
                body,
                scope: target.scope,
                delivery: PromptDelivery::State,
                order: 240,
            }))
        })
    }
}

/// The feature's parts. The Session part registers the source only when the
/// App Server has provided `ExecutionSettingsHost`.
pub fn feature() -> Feature {
    Feature::new("EnvironmentSettings")
        .session("EnvironmentSettings", |names| {
            Ok(Box::new(SettingsPlugin::new(names)?))
        })
        .branch("EnvironmentSettingsBranch", |names| {
            Ok(Box::new(SettingsBranchPlugin::new(names)?))
        })
}

pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(feature());
}

struct SettingsPlugin {
    host: Option<Key<ExecutionSettingsHost>>,
    prompts: Key<PromptRegistry>,
}
impl SettingsPlugin {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            host: names
                .registered::<ExecutionSettingsHost>()
                .then(|| names.key::<ExecutionSettingsHost>())
                .transpose()?,
            prompts: names.key()?,
        })
    }
}
impl DynPlugin for SettingsPlugin {
    fn name(&self) -> &str {
        "EnvironmentSettings"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut deps = vec![self.prompts.erase()];
        deps.extend(self.host.iter().map(Key::erase));
        deps
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let prompts = ctx.require_at(&self.prompts)?;
            let Some(host) = &self.host else {
                return Ok(Effect::Done);
            };
            let host = ctx.require_at(host)?;
            prompts.register(ctx, SOURCE, Arc::new(SettingsSource { host }))?;
            Ok(Effect::Done)
        })
    }
}

struct SettingsBranchPlugin {
    host: Option<Key<ExecutionSettingsHost>>,
    dispatcher: Key<Dispatcher>,
    configure: Key<dyn ConfigureControl>,
}
impl SettingsBranchPlugin {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            host: names
                .registered::<ExecutionSettingsHost>()
                .then(|| names.key::<ExecutionSettingsHost>())
                .transpose()?,
            dispatcher: names.key()?,
            configure: names.key()?,
        })
    }
}
impl DynPlugin for SettingsBranchPlugin {
    fn name(&self) -> &str {
        "EnvironmentSettingsBranch"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut deps = vec![self.dispatcher.erase(), self.configure.erase()];
        deps.extend(self.host.iter().map(Key::erase));
        deps
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            if self.host.is_none() {
                return Ok(Effect::Done);
            }
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let configure = ctx.require_at(&self.configure)?;
            let tasks = TaskTracker::new();

            let event_configure = configure.clone();
            let event_tasks = tasks.clone();
            ctx.listen::<SettingsChanged>()
                .name(settings_changes(SETTINGS_SOURCE))
                .on(move |_, _| {
                    let configure = event_configure.clone();
                    event_tasks.spawn(async move { adopt(configure).await });
                    async { Ok(None) }
                })?;

            let input_configure = configure.clone();
            let input_tasks = tasks.clone();
            dispatcher
                .follow(
                    ctx,
                    "EnvironmentSettings",
                    &["input/message"],
                    move |delivery| {
                        if let Delivery::Live(event) = delivery
                            && let EventKind::InputMessage(data) = &event.kind
                            && data.user.is_some()
                        {
                            let configure = input_configure.clone();
                            input_tasks.spawn(async move { adopt(configure).await });
                        }
                        Box::pin(async { Ok(()) })
                    },
                )
                .await?;

            ctx.effect(move || async move {
                tasks.close();
                tasks.wait().await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

async fn adopt(configure: Arc<dyn ConfigureControl>) {
    let update = ConfigureUpdate {
        sources: targets([SOURCE]),
        model: None,
    };
    if let Err(error) = configure.configure(update).await {
        tracing::warn!(%error, "execution settings were not adopted");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_off_states_normal_access() {
        assert_eq!(
            body(ExecutionSettings::default()),
            "Execution sandbox: off. New processes run with the host's normal access."
        );
    }

    #[test]
    fn sandbox_on_states_network_and_the_running_shell() {
        assert_eq!(
            body(ExecutionSettings {
                sandbox: true,
                network: false
            }),
            "Execution sandbox: on; network: off. New processes only; a running shell keeps its original boundary."
        );
        assert_eq!(
            body(ExecutionSettings {
                sandbox: true,
                network: true
            }),
            "Execution sandbox: on; network: on. New processes only; a running shell keeps its original boundary."
        );
    }
}

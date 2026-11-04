//! Model-facing execution settings: the sandbox and network switches as state.
//!
//! The App Server's `ExecutionSettings` owns the Settings document and
//! provides `ExecutionSettingsHost`. This capability registers
//! `dim/settings/execution` and adopts it on a periodic check while no turn is
//! open; an assembly without the host registers nothing.
use crate::{
    execution_settings::{ExecutionSettings, ExecutionSettingsHost},
    host::{HostError, HostFuture, LoadPrompt},
    prompt_registry::{PromptRegistry, targets},
    prompt_sources::sha256_version,
};
use dim_agent_runtime::{
    AgentBuilder, ConfigureControl, ConfigureUpdate, Delivery, Dispatcher, Feature, Names,
    PromptSourceTarget,
};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The prompt source of the execution switches.
pub const SOURCE: &str = "dim/settings/execution";

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
            let Some(host) = &self.host else {
                return Ok(Effect::Done);
            };
            let host = ctx.require_at(host)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let configure = ctx.require_at(&self.configure)?;
            let open = Arc::new(AtomicBool::new(false));
            let turn_open = open.clone();
            dispatcher
                .follow(
                    ctx,
                    "EnvironmentSettings",
                    &["turn/start", "turn/end"],
                    move |delivery| {
                        if let Delivery::Live(event) = delivery {
                            match &event.kind {
                                EventKind::TurnStart(_) => turn_open.store(true, Ordering::SeqCst),
                                EventKind::TurnEnd(_) => turn_open.store(false, Ordering::SeqCst),
                                _ => {}
                            }
                        }
                        Box::pin(async { Ok(()) })
                    },
                )
                .await?;

            let lifecycle = ctx.cancellation_token();
            let tasks = TaskTracker::new();
            tasks.spawn(async move {
                let Ok(mut last) = host.0.read(lifecycle.child_token()).await else {
                    return;
                };
                loop {
                    tokio::select! {
                        _ = lifecycle.cancelled() => return,
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    }
                    if open.load(Ordering::SeqCst) {
                        continue;
                    }
                    let current = match host.0.read(lifecycle.child_token()).await {
                        Ok(current) => current,
                        Err(error) => {
                            tracing::warn!(%error, "execution settings were not read");
                            continue;
                        }
                    };
                    if current == last {
                        continue;
                    }
                    let update = ConfigureUpdate {
                        sources: targets([SOURCE]),
                        model: None,
                    };
                    match configure.clone().configure(update).await {
                        Ok(_) => last = current,
                        Err(error) => {
                            tracing::warn!(%error, "execution settings were not adopted")
                        }
                    }
                }
            });
            ctx.effect(move || async move {
                tasks.close();
                tasks.wait().await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
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

use crate::{AimuxModels, driver::AimuxDriver};
use dim_agent_capabilities::host::*;
use dim_agent_runtime::{Names, SessionConfig, SessionIdentity};
use dim_agent_types::{Message, SystemPrompt, ToolDefinition};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use rutis::{Ctx, Plugin};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Process-owned request execution; no catalog or Session dependencies.
pub struct AimuxPlugin;
impl Plugin for AimuxPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "Aimux"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let driver = AimuxDriver::new(ctx.cancellation_token())
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
        let owned = driver.clone();
        if let Err(error) = ctx.effect(move || async move {
            owned.close().await;
            Ok(())
        }) {
            driver.close().await;
            return Err(error);
        }
        ctx.provide(ModelHost(driver))?;
        Ok(())
    }
}

/// The API-key profile supplies model selection and request-local connections;
/// execution capabilities are independent of this provider choice.
pub struct ConfiguredModelsPlugin(pub Arc<AimuxModels>);
impl Plugin for ConfiguredModelsPlugin {
    /// The shared name and the run for auxiliary calls need the model host;
    /// Aimux provides it and depends on nothing here.
    type Deps = (Arc<ModelHost>,);
    fn name(&self) -> &str {
        "ConfiguredModels"
    }
    async fn apply(&self, ctx: &Ctx, (host,): Self::Deps) -> rutis::Result {
        ctx.provide_as::<AimuxModels>(self.0.clone())?;
        let resolver = Arc::new(ConfiguredConnection {
            models: self.0.clone(),
            generation: ctx.cancellation_token(),
        });
        ctx.provide(ProviderConnectionResolver(resolver.clone()))?;
        ctx.provide(ModelSelectionHost(resolver.clone()))?;
        let auxiliary = Arc::new(crate::auxiliary::ConfiguredAuxiliary {
            models: self.0.clone(),
            connection: resolver,
            host,
        });
        dim_agent_capabilities::auxiliary_model::provide(ctx, auxiliary)?;
        Ok(())
    }
}
struct ConfiguredConnection {
    models: Arc<AimuxModels>,
    generation: CancellationToken,
}
impl ResolveModelSelection for ConfiguredConnection {
    fn resolve(
        &self,
        explicit: Option<dim_agent_types::ModelId>,
        _: Option<std::path::PathBuf>,
        cancel: CancellationToken,
    ) -> HostFuture<'_, dim_agent_types::ModelSelection> {
        Box::pin(async move {
            if self.generation.is_cancelled() {
                return Err(HostError::classified(
                    HostErrorKind::OwnerClosed,
                    "Model catalog owner is closed",
                ));
            }
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled("Model selection cancelled".into()));
            }
            let id = explicit.unwrap_or_else(|| self.models.default_model().clone());
            let model = self.models.get(&id.provider_id, &id.model_id)?;
            // Record the protocol beside the adopted configuration, in the
            // same metadata member Provider records it, so the existing reads
            // derive the preferences the configuration sends.
            Ok(dim_agent_types::ModelSelection {
                provider_id: id.provider_id,
                model_id: id.model_id,
                capability_snapshot: None,
                config: Some(model.defaults.clone()),
                metadata: Some(dim_agent_types::RequestMetadata {
                    extensions: serde_json::Map::from_iter([(
                        "providerProtocol".into(),
                        serde_json::json!(model.kind),
                    )]),
                    ..Default::default()
                }),
            })
        })
    }
}
impl ResolveModelConnection for ConfiguredConnection {
    fn resolve(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async move {
            if self.generation.is_cancelled() {
                return Err(HostError::classified(
                    HostErrorKind::OwnerClosed,
                    "Model catalog owner is closed",
                ));
            }
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled(
                    "Connection resolution cancelled".into(),
                ));
            }
            let model = self.models.get(&request.provider_id, &request.model_id)?;
            let credential = model
                .api_key_env
                .as_deref()
                .map(|name| {
                    let key = std::env::var(name)
                        .ok()
                        .filter(|key| !key.is_empty())
                        .ok_or_else(|| {
                            HostError::classified(
                                HostErrorKind::InvalidRequest,
                                format!("Missing credential environment variable: {name}"),
                            )
                        })?;
                    http::HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                        HostError::classified(
                            HostErrorKind::InvalidRequest,
                            "Invalid credential header",
                        )
                    })?;
                    Ok(key)
                })
                .transpose()?;
            Ok(ResolvedModelConnection {
                failure_policy: None,
                recovery: None,
                connection: ModelConnection::Http(HttpModelConnection {
                    protocol: model.kind,
                    base_url: model.base_url.clone(),
                    network: Default::default(),
                    headers: model.headers.clone(),
                    bearer: if matches!(
                        model.kind,
                        ModelProtocol::Anthropic | ModelProtocol::Gemini
                    ) {
                        None
                    } else {
                        credential.clone()
                    },
                    api_key: if matches!(
                        model.kind,
                        ModelProtocol::Anthropic | ModelProtocol::Gemini
                    ) {
                        credential
                    } else {
                        None
                    },
                    preset: model.preset.clone(),
                }),
            })
        })
    }
}

/// Request preparation stays on the calling side of the process adapter.
pub struct ConfiguredModelPlugin {
    prepare: Key<PrepareModelHost>,
    models: Key<AimuxModels>,
    config: Key<SessionConfig>,
    identity: Key<SessionIdentity>,
}
impl ConfiguredModelPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            prepare: names.key()?,
            models: names.key()?,
            config: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for ConfiguredModelPlugin {
    fn name(&self) -> &str {
        "ConfiguredModel"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.models.erase(),
            self.config.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let models = ctx.require_at(&self.models)?;
            let config = ctx.require_at(&self.config)?;
            let identity = ctx.require_at(&self.identity)?;
            let defaults = models
                .get(&config.model.provider_id, &config.model.model_id)
                .map_err(|error| {
                    rutis::Error::from(ErrorKind::Validation {
                        issues: vec![error.to_string()],
                    })
                })?
                .defaults
                .clone();
            ctx.provide_at(
                self.prepare.clone(),
                Arc::new(PrepareModelHost(Arc::new(Prepare {
                    config,
                    identity,
                    defaults,
                }))),
            )?;
            Ok(Effect::Done)
        })
    }
}

/// The request metadata member carrying the Timeline request id, so a recording
/// of the wire exchange names the request it belongs to.
pub(crate) const REQUEST_ID_EXTENSION: &str = "requestId";

struct Prepare {
    config: Arc<SessionConfig>,
    identity: Arc<SessionIdentity>,
    defaults: dim_agent_types::ModelConfig,
}
impl PrepareModel for Prepare {
    fn prepare(
        &self,
        turn_id: &str,
        request_id: &str,
        _: &[Arc<Message>],
        available_tools: &[ToolDefinition],
    ) -> Result<ModelSettings, HostError> {
        let mut settings =
            ModelSettings::new(&self.config.model.provider_id, &self.config.model.model_id);
        settings.capability_snapshot = self.config.model.capability_snapshot.clone();
        settings.config = Some(
            self.config
                .model
                .config
                .as_ref()
                .unwrap_or(&self.defaults)
                .clone(),
        );
        settings.system = (!self.config.system_prompt.is_empty())
            .then(|| SystemPrompt::Text(self.config.system_prompt.clone()));
        settings.tools = Some(available_tools.to_vec());
        let mut metadata = self.config.model.metadata.clone().unwrap_or_default();
        metadata.session_id = Some(self.identity.session_id.clone());
        metadata.turn_id = Some(turn_id.into());
        metadata
            .extensions
            .insert(REQUEST_ID_EXTENSION.into(), request_id.into());
        settings.metadata = Some(metadata);
        Ok(settings)
    }
}

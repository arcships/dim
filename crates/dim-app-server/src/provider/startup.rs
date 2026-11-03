use super::*;
use dim_agent_runtime::Session;
use dim_agent_types::{ModelConfiguration, StartupAuth, StartupConnectionSource};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartupInput {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub connection: Option<StartupConnectionInput>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub context_window: Option<u64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub auto_compact_window: Option<AutoCompactWindow>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartupConnectionInput {
    pub api_format: ModelProtocol,
    pub base_url: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub api_key: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl std::fmt::Debug for StartupConnectionInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StartupConnectionInput([redacted])")
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum AutoCompactWindow {
    Auto(AutoWindow),
    Tokens(u64),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoWindow {
    Auto,
}

struct Binding {
    source: StartupConnectionSource,
    connection: HttpModelConnection,
}

fn same_destination(a: &StartupConnectionSource, b: &StartupConnectionSource) -> bool {
    a.api_format == b.api_format
        && a.base_url == b.base_url
        && a.auth == b.auth
        && a.header_names == b.header_names
}

#[derive(Default)]
pub(super) struct StartupBindings {
    connections: HashMap<String, Binding>,
    owners: HashMap<String, HashSet<String>>,
    watched: HashMap<String, uuid::Uuid>,
}

/// Holds an unclaimed binding until Session creation or adoption succeeds.
pub struct StartupSelection {
    pub model: ModelSelection,
    owner: Arc<ProviderService>,
    pending: Option<String>,
}

impl Drop for StartupSelection {
    fn drop(&mut self) {
        if let Some(id) = &self.pending {
            let mut bindings = self.owner.startup.lock().unwrap();
            if !bindings.owners.contains_key(id) {
                bindings.connections.remove(id);
            }
        }
    }
}

impl StartupConnectionInput {
    fn admit(self) -> Result<(StartupConnectionSource, HttpModelConnection), HostError> {
        if self.api_format == ModelProtocol::Bedrock {
            return Err(failure(
                "Direct startup does not support signed Bedrock; use a configured Provider",
            ));
        }
        let url =
            reqwest::Url::parse(&self.base_url).map_err(|_| failure("Invalid startup base URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || self.base_url.trim() != self.base_url
            || self.base_url.bytes().any(|byte| byte.is_ascii_control())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(failure(
                "Startup base URL must be HTTP(S), without user info, query or fragment",
            ));
        }
        if self.api_key.is_some() && self.auth_token.is_some() {
            return Err(failure(
                "Supply either DIM_API_KEY or DIM_AUTH_TOKEN, not both",
            ));
        }
        let auth = if self.api_key.is_some() {
            StartupAuth::ApiKey
        } else if self.auth_token.is_some() {
            StartupAuth::Bearer
        } else {
            StartupAuth::Anonymous
        };
        for credential in [&self.api_key, &self.auth_token].into_iter().flatten() {
            if credential.is_empty() || reqwest::header::HeaderValue::from_str(credential).is_err()
            {
                return Err(failure("Invalid startup credential"));
            }
        }
        let mut headers = HashMap::new();
        for (name, value) in self.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| failure("Invalid startup header name"))?
                .as_str()
                .to_owned();
            let value = value.trim_matches([' ', '\t']);
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| failure("Invalid startup header value"))?;
            if headers.insert(name, value.to_owned()).is_some() {
                return Err(failure("Duplicate startup header name"));
            }
        }
        let generated_auth = match (auth, self.api_format) {
            (StartupAuth::Anonymous, _) => None,
            (StartupAuth::ApiKey, ModelProtocol::Anthropic) => Some("x-api-key"),
            (StartupAuth::ApiKey, ModelProtocol::Gemini) => Some("x-goog-api-key"),
            _ => Some("authorization"),
        };
        if generated_auth.is_some_and(|name| headers.contains_key(name)) {
            return Err(failure(
                "Custom header conflicts with generated startup authentication",
            ));
        }
        let mut header_names: Vec<_> = headers.keys().cloned().collect();
        header_names.sort();
        let source = StartupConnectionSource {
            id: uuid::Uuid::new_v4().to_string(),
            api_format: self.api_format,
            base_url: url.as_str().trim_end_matches('/').to_owned(),
            auth,
            header_names,
        };
        let connection = HttpModelConnection {
            protocol: source.api_format,
            base_url: source.base_url.clone(),
            network: ModelNetwork::default(),
            headers,
            bearer: self.auth_token,
            api_key: self.api_key,
            preset: None,
        };
        Ok((source, connection))
    }
}

impl ProviderService {
    /// Resolves a startup model and validates its Session-only declarations.
    /// No Provider, preference or credential record is written.
    pub async fn startup_configuration(
        self: &Arc<Self>,
        target: crate::protocol::ModelSelection,
        cwd: &Path,
        preferences: ModelPreferences,
        input: StartupInput,
        current: Option<&ModelConfiguration>,
        cancel: &CancellationToken,
    ) -> Result<StartupSelection, HostError> {
        self.check(cancel)?;
        for value in [input.context_window, input.max_output_tokens]
            .into_iter()
            .flatten()
        {
            if value == 0 || value > MAX_MODEL_TOKENS {
                return Err(failure("Model limits must be positive safe JSON integers"));
            }
        }
        if target
            .provider
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
            || target
                .model_id
                .as_ref()
                .is_some_and(|id| id.trim().is_empty())
            || (target.provider.is_some() && target.model_id.is_none())
        {
            return Err(failure(
                "Supply a non-empty model ID, optionally with its Provider",
            ));
        }
        if input.connection.is_some() && target.provider.is_some() {
            return Err(failure(
                "Direct startup cannot be combined with an explicit Provider",
            ));
        }
        let admitted = match input.connection {
            Some(connection) => Some(connection.admit()?),
            None if target.provider.is_none() => current
                .and_then(|model| model.metadata.as_ref())
                .and_then(|metadata| metadata.startup.as_ref())
                .and_then(|startup| startup.connection.as_ref())
                .map(|source| {
                    self.startup_connection(source)
                        .map(|connection| (source.clone(), connection))
                })
                .transpose()?,
            None => None,
        };
        let mut model = if let Some((source, _)) = &admitted {
            let model_id = target
                .model_id
                .clone()
                .or_else(|| current.map(|model| model.model_id.clone()))
                .ok_or_else(|| failure("Direct startup requires --model or DIM_MODEL"))?;
            let previous = current.filter(|model| {
                model.model_id == model_id
                    && provider_protocol(model.metadata.as_ref()) == Some(source.api_format)
                    && model
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.startup.as_ref())
                        .is_some_and(|startup| startup.connection.is_some())
            });
            let mut descriptor = serde_json::json!({"id": model_id});
            if let Some(limit) = input.context_window {
                descriptor["contextWindow"] = limit.into();
            }
            if let Some(limit) = input.max_output_tokens {
                descriptor["maxOutputTokens"] = limit.into();
            }
            let provider_id = match source.api_format {
                ModelProtocol::Anthropic => "anthropic",
                ModelProtocol::OpenaiChat | ModelProtocol::OpenaiResponses => "openai",
                ModelProtocol::OpenaiCompatible => "custom-provider",
                ModelProtocol::Gemini => "google",
                ModelProtocol::Xai | ModelProtocol::XaiResponses => "xai",
                ModelProtocol::Bedrock => "amazon-bedrock",
            };
            let projection = ProviderProjection {
                provider_id: provider_id.into(),
                models: self
                    .bundled
                    .bundled_models(provider_id)
                    .unwrap_or_default()
                    .iter()
                    .find(|model| model.model_id == model_id)
                    .cloned()
                    .into_iter()
                    .collect(),
                ..Default::default()
            };
            let entry = self
                .bundled
                .project(&projection, &descriptor)
                .map_err(failure)?;
            let mut snapshot = match previous {
                Some(model) => model.capability_snapshot.clone().ok_or_else(|| failure("Model capabilities are not adopted"))?,
                None => ModelCapabilitySnapshot {
                target: ModelId { provider_id: source.id.clone(), model_id: model_id.clone() },
                capabilities: entry.capabilities.declared()
                    .ok_or_else(|| failure(format!("{}; supply DIM_MAX_CONTEXT_TOKENS and DIM_MAX_OUTPUT_TOKENS or configure the model descriptor", undeclared_limits(&entry.capabilities))))?,
                reasoning: entry.metadata.as_ref().and_then(|metadata| metadata.reasoning.clone()),
                service_tier: entry.metadata.as_ref().and_then(|metadata| metadata.service_tier.clone()),
                },
            };
            snapshot.target.provider_id = source.id.clone();
            let base = previous
                .and_then(|model| model.config.clone())
                .unwrap_or_else(|| ModelConfig {
                    max_tokens: Some(snapshot.capabilities.max_output_tokens as f64),
                    ..Default::default()
                });
            let base = preferences::catalog_defaults(&base, &snapshot, source.api_format);
            let config = preferences::configure(&preferences, &snapshot, source.api_format, &base)
                .map_err(failure)?;
            ModelSelection {
                provider_id: source.id.clone(),
                model_id,
                capability_snapshot: Some(snapshot),
                config: Some(config),
                metadata: Some(dim_agent_types::RequestMetadata {
                    startup: previous
                        .and_then(|model| model.metadata.as_ref())
                        .and_then(|metadata| metadata.startup.clone()),
                    extensions: serde_json::Map::from_iter([(
                        "providerProtocol".into(),
                        serde_json::json!(source.api_format),
                    )]),
                    ..Default::default()
                }),
            }
        } else if let Some(current) = current.filter(|_| target.model_id.is_none()) {
            let protocol = provider_protocol(current.metadata.as_ref())
                .ok_or_else(|| failure("Missing adopted model protocol"))?;
            let snapshot = current
                .capability_snapshot
                .as_ref()
                .ok_or_else(|| failure("Model capabilities are not adopted"))?;
            let config = preferences::configure(
                &preferences,
                snapshot,
                protocol,
                &current.config.clone().unwrap_or_default(),
            )
            .map_err(failure)?;
            ModelSelection {
                provider_id: current.provider_id.clone(),
                model_id: current.model_id.clone(),
                capability_snapshot: current.capability_snapshot.clone(),
                config: Some(config),
                metadata: current.metadata.clone(),
            }
        } else {
            let explicit = match (target.provider, target.model_id) {
                (Some(provider_id), Some(model_id)) => Some(ModelId {
                    provider_id,
                    model_id,
                }),
                (None, Some(model_id)) => Some(
                    self.startup_target(
                        &model_id,
                        cwd,
                        (input.context_window, input.max_output_tokens),
                        cancel,
                    )
                    .await?,
                ),
                (None, None) => None,
                _ => unreachable!("target validated"),
            };
            let selected = self
                .configuration_with_limits(
                    explicit,
                    Some(cwd),
                    ModelPreferences::default(),
                    (input.context_window, input.max_output_tokens),
                    cancel,
                )
                .await?;
            let view = self
                .management(Some(selected.provider_id.clone()), cancel)
                .await
                .map_err(failure)?;
            let saved = view
                .providers
                .iter()
                .find(|provider| provider.provider_id == selected.provider_id)
                .and_then(|provider| provider.preferences.get(&selected.model_id))
                .cloned()
                .unwrap_or_default();
            let prefs = ModelPreferences {
                reasoning_effort: preferences.reasoning_effort.or(saved.reasoning_effort),
                service_tier: preferences.service_tier.or(saved.service_tier),
            };
            let protocol = provider_protocol(selected.metadata.as_ref())
                .ok_or_else(|| failure("Missing adopted model protocol"))?;
            let config = preferences::configure(
                &prefs,
                selected
                    .capability_snapshot
                    .as_ref()
                    .expect("Provider adopts capabilities"),
                protocol,
                selected
                    .config
                    .as_ref()
                    .expect("Provider adopts configuration"),
            )
            .map_err(failure)?;
            ModelSelection {
                config: Some(config),
                ..selected
            }
        };
        let snapshot = model
            .capability_snapshot
            .as_mut()
            .expect("Provider adopts capabilities");
        if let Some(limit) = input.context_window {
            snapshot.capabilities.context_window = limit;
        }
        if let Some(limit) = input.max_output_tokens {
            snapshot.capabilities.max_output_tokens = limit;
            let config = model.config.get_or_insert_default();
            config.max_tokens = Some(
                config
                    .max_tokens
                    .map_or(limit as f64, |budget| budget.min(limit as f64)),
            );
        }
        if snapshot.capabilities.max_output_tokens > snapshot.capabilities.context_window {
            return Err(failure("Output capacity cannot exceed context capacity"));
        }
        let metadata = model.metadata.get_or_insert_default();
        let startup = metadata.startup.get_or_insert_default();
        match input.auto_compact_window {
            Some(AutoCompactWindow::Auto(_)) => startup.auto_compact_window = None,
            Some(AutoCompactWindow::Tokens(tokens)) => {
                if tokens == 0 || tokens > snapshot.capabilities.context_window {
                    return Err(failure(
                        "Auto-compaction window must be positive and no larger than the context capacity",
                    ));
                }
                startup.auto_compact_window = Some(tokens);
            }
            None => {}
        }
        if startup
            .auto_compact_window
            .is_some_and(|window| window > snapshot.capabilities.context_window)
        {
            return Err(failure(
                "Auto-compaction window exceeds the adopted context capacity",
            ));
        }
        let pending = if let Some((mut source, connection)) = admitted {
            if let Some(previous) = current
                .and_then(|model| model.metadata.as_ref())
                .and_then(|metadata| metadata.startup.as_ref())
                .and_then(|startup| startup.connection.as_ref())
                && same_destination(previous, &source)
                && !self
                    .startup
                    .lock()
                    .unwrap()
                    .connections
                    .contains_key(&previous.id)
            {
                source.id = previous.id.clone();
                model.provider_id = source.id.clone();
                snapshot.target.provider_id = source.id.clone();
            }
            let id = source.id.clone();
            startup.connection = Some(source.clone());
            self.startup
                .lock()
                .unwrap()
                .connections
                .insert(id.clone(), Binding { source, connection });
            Some(id)
        } else {
            if let Some(source) = &startup.connection {
                self.startup_connection(source)?;
            }
            None
        };
        Ok(StartupSelection {
            model,
            owner: self.clone(),
            pending,
        })
    }

    async fn startup_target(
        self: &Arc<Self>,
        model_id: &str,
        cwd: &Path,
        limits: (Option<u64>, Option<u64>),
        cancel: &CancellationToken,
    ) -> Result<ModelId, HostError> {
        let effective = self
            .configuration_with_limits(None, Some(cwd), ModelPreferences::default(), limits, cancel)
            .await;
        self.check(cancel)?;
        let mut catalog_error = None;
        if let Err(error @ HostError::Cancelled(_)) = effective {
            return Err(error);
        }
        if let Ok(effective) = effective {
            let catalog = self
                .catalog_for(effective.provider_id.clone(), cancel)
                .await;
            self.check(cancel)?;
            match catalog {
                Err(error @ HostError::Cancelled(_)) => return Err(error),
                Ok(Some(mut catalog)) => {
                    catalog.apply_startup_limits(limits.0, limits.1);
                    if catalog.selectable(model_id).is_some() {
                        return Ok(ModelId {
                            provider_id: effective.provider_id,
                            model_id: model_id.into(),
                        });
                    }
                }
                Err(error) => catalog_error = Some(error),
                Ok(None) => {}
            }
        }
        let mut matches = Vec::new();
        let mut providers: HashSet<_> = self
            .list(cancel)
            .await?
            .providers
            .into_iter()
            .map(|provider| provider.provider_id)
            .collect();
        providers.extend(
            self.oauth
                .providers(cancel)
                .await
                .map_err(failure)?
                .into_iter()
                .map(|provider| provider.provider_id),
        );
        for provider_id in providers {
            let catalog = self.catalog_for(provider_id.clone(), cancel).await;
            self.check(cancel)?;
            let mut catalog = match catalog {
                Ok(Some(catalog)) => catalog,
                Err(error @ HostError::Cancelled(_)) => return Err(error),
                Err(error) => {
                    catalog_error.get_or_insert(error);
                    continue;
                }
                Ok(None) => continue,
            };
            catalog.apply_startup_limits(limits.0, limits.1);
            if catalog.selectable(model_id).is_some() {
                matches.push(ModelId {
                    provider_id,
                    model_id: model_id.into(),
                });
            }
        }
        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => {
                let message = "Model is unavailable; configure its Provider or supply a direct startup connection";
                Err(failure(match catalog_error {
                    Some(error) => format!("{message}; catalog error: {error}"),
                    None => message.to_owned(),
                }))
            }
            _ => Err(failure(
                "Model ID is ambiguous; specify --provider or a direct startup connection",
            )),
        }
    }

    pub(crate) fn startup_connection(
        &self,
        source: &StartupConnectionSource,
    ) -> Result<HttpModelConnection, HostError> {
        self.startup.lock().unwrap().connections.get(&source.id)
            .filter(|binding| binding.source == *source)
            .map(|binding| binding.connection.clone())
            .ok_or_else(|| failure("Temporary model connection is not bound; resume with the matching DIM_BASE_URL, DIM_API_FORMAT and credentials"))
    }

    pub(crate) fn retain_startup(
        self: &Arc<Self>,
        session: &Session,
        model: &ModelSelection,
    ) -> Result<(), HostError> {
        let Some(source) = model
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.startup.as_ref())
            .and_then(|startup| startup.connection.as_ref())
        else {
            return Ok(());
        };
        let connection = self.startup_connection(source)?;
        {
            let mut bindings = self.startup.lock().unwrap();
            bindings
                .connections
                .entry(source.id.clone())
                .or_insert_with(|| Binding {
                    source: source.clone(),
                    connection: connection.clone(),
                });
            bindings
                .owners
                .entry(source.id.clone())
                .or_default()
                .insert(session.id().to_owned());
            for branch in session.branches() {
                if let Ok(Some(state)) =
                    session.get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&branch.id)
                    && let Some(previous) = state
                        .configuration
                        .as_ref()
                        .and_then(|model| model.metadata.as_ref())
                        .and_then(|metadata| metadata.startup.as_ref())
                        .and_then(|startup| startup.connection.as_ref())
                    && same_destination(previous, source)
                {
                    bindings
                        .connections
                        .entry(previous.id.clone())
                        .or_insert_with(|| Binding {
                            source: previous.clone(),
                            connection: connection.clone(),
                        });
                    bindings
                        .owners
                        .entry(previous.id.clone())
                        .or_default()
                        .insert(session.id().to_owned());
                }
            }
        }
        self.watch_startup(session.clone());
        Ok(())
    }

    pub(super) fn restore_startup(self: &Arc<Self>, session: Session) {
        let mut bindings = self.startup.lock().unwrap();
        for branch in session.branches() {
            if let Ok(Some(state)) =
                session.get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&branch.id)
                && let Some(source) = state
                    .configuration
                    .as_ref()
                    .and_then(|model| model.metadata.as_ref())
                    .and_then(|metadata| metadata.startup.as_ref())
                    .and_then(|startup| startup.connection.as_ref())
            {
                bindings
                    .owners
                    .entry(source.id.clone())
                    .or_default()
                    .insert(session.id().to_owned());
            }
        }
        drop(bindings);
        self.watch_startup(session);
    }

    pub(super) fn watch_startup(self: &Arc<Self>, session: Session) {
        let id = session.id().to_owned();
        let generation = uuid::Uuid::new_v4();
        {
            let mut bindings = self.startup.lock().unwrap();
            if !bindings.owners.values().any(|owners| owners.contains(&id)) {
                return;
            }
            bindings.watched.insert(id.clone(), generation);
        }
        let owner = self.clone();
        self.tasks.spawn(async move {
            tokio::select! {
                _ = owner.stop.cancelled() => {}
                _ = session.closed() => {
                    let mut bindings = owner.startup.lock().unwrap();
                    if bindings.watched.get(&id) == Some(&generation) {
                        bindings.watched.remove(&id);
                        if !session.is_unloading() { bindings.forget(&id); }
                    }
                }
            }
        });
    }

    pub(super) fn forget_startup(&self, session_id: &str) {
        let mut bindings = self.startup.lock().unwrap();
        bindings.watched.remove(session_id);
        bindings.forget(session_id);
    }
}

impl StartupBindings {
    fn forget(&mut self, session_id: &str) {
        self.owners.retain(|id, owners| {
            owners.remove(session_id);
            if owners.is_empty() {
                self.connections.remove(id);
                false
            } else {
                true
            }
        });
    }
}

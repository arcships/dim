use super::{
    CredentialAuthority, ProviderService, ProviderSummary, StaticAuth,
    startup::{self, StartupConnectionInput},
};
use dim_agent_capabilities::methods::{Caller, RpcError};
use dim_agent_types::{ModelProtocol, ModelSelection, RequestMetadata, StartupConnectionSource};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio_util::sync::CancellationToken;

const LOGICAL_PROVIDER: &str = "scopedRouteProvider";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RouteView {
    provider_id: String,
    protocol: ModelProtocol,
    base_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SetRoute {
    provider_id: String,
    connection: StartupConnectionInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RouteTarget {
    provider_id: String,
}

pub(super) struct Route {
    source: StartupConnectionSource,
    connection: dim_agent_capabilities::host::HttpModelConnection,
}

pub(super) type Routes = HashMap<String, HashMap<String, Option<Route>>>;

fn configurable(info: ProviderSummary) -> Option<RouteView> {
    if info.credential_authority != CredentialAuthority::Static
        || !info.enabled
        || (info.auth != StaticAuth::Anonymous && !info.credential_configured)
    {
        return None;
    }
    let protocol = match info.driver_kind.as_str() {
        "openai-compatible" | "custom" => ModelProtocol::OpenaiCompatible,
        "openai-responses" => ModelProtocol::OpenaiResponses,
        "anthropic" => ModelProtocol::Anthropic,
        _ => return None,
    };
    Some(RouteView {
        provider_id: info.provider_id,
        protocol,
        base_url: Some(startup::normalize_base_url(&info.base_url).ok()?),
    })
}

impl ProviderService {
    pub(super) async fn scoped_routes(
        self: &Arc<Self>,
        caller: &Caller,
        cancel: &CancellationToken,
    ) -> Result<Vec<RouteView>, RpcError> {
        let catalog = self.list(cancel).await.map_err(super::management::error)?;
        let bindings = self.startup.lock().unwrap();
        let routes = bindings.routes.get(caller.id());
        Ok(catalog
            .providers
            .into_iter()
            .filter_map(configurable)
            .map(|mut row| {
                if let Some(route) = routes.and_then(|routes| routes.get(&row.provider_id)) {
                    row.base_url = route.as_ref().map(|route| route.source.base_url.clone());
                }
                row
            })
            .collect())
    }

    async fn routing_target(
        self: &Arc<Self>,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<RouteView>, RpcError> {
        if id.trim().is_empty() {
            return Err(RpcError::invalid("Provider id is required"));
        }
        let catalog = self.list(cancel).await.map_err(super::management::error)?;
        if catalog.errors.iter().any(|error| error.provider_id == id) {
            return Err(RpcError::invalid("Provider configuration is unavailable"));
        }
        catalog
            .providers
            .into_iter()
            .find(|info| info.provider_id == id)
            .map(|info| {
                configurable(info)
                    .ok_or_else(|| RpcError::invalid("Provider does not support temporary routing"))
            })
            .transpose()
    }

    pub(super) async fn set_scoped_route(
        self: &Arc<Self>,
        caller: &Caller,
        request: SetRoute,
        cancel: &CancellationToken,
    ) -> Result<(), RpcError> {
        let row = self
            .routing_target(&request.provider_id, cancel)
            .await?
            .ok_or_else(|| RpcError::invalid("Unknown Provider"))?;
        if request.connection.api_format != row.protocol {
            return Err(RpcError::invalid("Unsupported Provider protocol"));
        }
        let (source, mut connection) = request
            .connection
            .admit()
            .map_err(|error| RpcError::invalid(error.to_string()))?;
        // A temporary route on the built-in xai provider is still xAI: the
        // route replaces the endpoint, not the vendor the protocol does not
        // name.
        if request.provider_id == "xai" {
            connection.preset = Some("xai".into());
        }
        self.put_scoped_route(
            caller,
            request.provider_id,
            Some(Route { source, connection }),
            cancel,
        )?;
        Ok(())
    }

    pub(super) async fn disable_scoped_route(
        self: &Arc<Self>,
        caller: &Caller,
        request: RouteTarget,
        cancel: &CancellationToken,
    ) -> Result<bool, RpcError> {
        let has_override = self
            .startup
            .lock()
            .unwrap()
            .routes
            .get(caller.id())
            .is_some_and(|routes| routes.contains_key(&request.provider_id));
        if has_override
            || self
                .routing_target(&request.provider_id, cancel)
                .await?
                .is_some()
        {
            return self.put_scoped_route(caller, request.provider_id, None, cancel);
        }
        Ok(false)
    }

    fn put_scoped_route(
        self: &Arc<Self>,
        caller: &Caller,
        id: String,
        route: Option<Route>,
        cancel: &CancellationToken,
    ) -> Result<bool, RpcError> {
        let mut bindings = self.startup.lock().unwrap();
        if cancel.is_cancelled() {
            return Err(RpcError::interrupted());
        }
        let scope = caller.id();
        if !bindings.routes.contains_key(scope) {
            let owner = self.clone();
            let scope = scope.to_owned();
            let closed = caller.closed();
            self.tasks.spawn(async move {
                tokio::select! {
                    _ = closed => {},
                    _ = owner.stop.cancelled() => {},
                }
                let mut bindings = owner.startup.lock().unwrap();
                bindings.routes.remove(&scope);
                bindings
                    .connections
                    .retain(|_, binding| binding.scope.as_deref() != Some(scope.as_str()));
            });
        }
        let routes = bindings.routes.entry(scope.to_owned()).or_default();
        let sets_route = route.is_some();
        let before = routes.insert(id, route);
        Ok(sets_route || before.is_none_or(|previous| previous.is_some()))
    }

    pub(crate) fn scoped_connection_owner(
        &self,
        scope: &str,
        metadata: Option<&RequestMetadata>,
        recover_shared: bool,
    ) -> Result<bool, RpcError> {
        let Some(source) = metadata
            .filter(|metadata| metadata.extensions.contains_key(LOGICAL_PROVIDER))
            .and_then(|metadata| metadata.startup.as_ref())
            .and_then(|startup| startup.connection.as_ref())
        else {
            return Ok(false);
        };
        let bindings = self.startup.lock().unwrap();
        match bindings.connections.get(&source.id) {
            Some(binding) if binding.scope.as_deref() == Some(scope) => Ok(false),
            Some(_) => Err(RpcError::service(
                "provider_route_conflict",
                "Session has a temporary route owned by another connection",
            )),
            None if recover_shared => Ok(true),
            None => Err(RpcError::service(
                "provider_route_unavailable",
                "Session's temporary model route has expired",
            )),
        }
    }

    pub(crate) fn check_fork_route(
        &self,
        scope: &str,
        provider_id: &str,
        metadata: Option<&RequestMetadata>,
    ) -> Result<(), RpcError> {
        self.scoped_connection_owner(scope, metadata, false)?;
        let logical = logical_provider(metadata);
        let bindings = self.startup.lock().unwrap();
        let Some(route) = bindings
            .routes
            .get(scope)
            .and_then(|routes| routes.get(logical.unwrap_or(provider_id)))
        else {
            return Ok(());
        };
        let route = route.as_ref().ok_or_else(disabled)?;
        let inherited = logical
            .and(metadata)
            .and_then(|metadata| metadata.startup.as_ref())
            .and_then(|startup| startup.connection.as_ref())
            .and_then(|source| bindings.connections.get(&source.id));
        if inherited.is_some_and(|binding| {
            startup::same_destination(&binding.source, &route.source)
                && binding.connection.headers == route.connection.headers
                && binding.connection.bearer == route.connection.bearer
                && binding.connection.api_key == route.connection.api_key
        }) {
            return Ok(());
        }
        Err(RpcError::service(
            "provider_route_conflict",
            "Select the current route before forking from its adoption",
        ))
    }

    pub(crate) fn scoped_model(
        self: &Arc<Self>,
        scope: &str,
        model: &ModelSelection,
    ) -> Result<Option<startup::StartupSelection>, RpcError> {
        let mut bindings = self.startup.lock().unwrap();
        let Some(change) = bindings
            .routes
            .get(scope)
            .and_then(|routes| routes.get(&model.provider_id))
        else {
            return Ok(None);
        };
        let route = change.as_ref().ok_or_else(disabled)?;
        if super::provider_protocol(model.metadata.as_ref()) != Some(route.source.api_format) {
            return Err(RpcError::invalid(
                "Selected model does not use the temporary route's protocol",
            ));
        }

        let mut model = model.clone();
        let mut source = route.source.clone();
        source.id = uuid::Uuid::new_v4().to_string();
        let snapshot = model
            .capability_snapshot
            .as_mut()
            .ok_or_else(|| RpcError::invalid("Selected model has no capability declaration"))?;
        snapshot.target.provider_id = source.id.clone();
        let metadata = model.metadata.get_or_insert_default();
        metadata
            .extensions
            .insert(LOGICAL_PROVIDER.into(), model.provider_id.into());
        metadata.startup.get_or_insert_default().connection = Some(source.clone());
        model.provider_id = source.id.clone();
        let id = source.id.clone();
        let binding = startup::Binding {
            source,
            connection: route.connection.clone(),
            scope: Some(scope.to_owned()),
        };
        bindings.connections.insert(id.clone(), binding);
        Ok(Some(startup::StartupSelection {
            model,
            owner: self.clone(),
            pending: Some(id),
        }))
    }
}

fn disabled() -> RpcError {
    RpcError::service(
        "provider_route_disabled",
        "Selected Provider is disabled for this connection",
    )
}

pub(crate) fn logical_provider(metadata: Option<&RequestMetadata>) -> Option<&str> {
    metadata?.extensions.get(LOGICAL_PROVIDER)?.as_str()
}

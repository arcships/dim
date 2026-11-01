//! Session-scoped paths the user allowed for this Session, and the
//! `session.grants.*` methods that manage them.
use crate::storage::SessionStore;
use dim_agent_capabilities::{
    methods::{Interruption, MethodDecl, RpcError, SessionMethods, params, reply},
    session_grants::{ReadSessionGrants, SessionGrantsHost, resolve_grant_path},
};
use dim_agent_runtime::{AgentBuilder, Names, SessionConfig, SessionIdentity};
use dim_agent_types::grant::{GrantKind, GrantMode, SessionGrant};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(
        dim_agent_runtime::Feature::new("SessionGrants").session("SessionGrants", |names| {
            Ok(Box::new(SessionGrantsPlugin::new(names)?))
        }),
    );
}

struct StoreGrants {
    store: Arc<SessionStore>,
    session_id: String,
}
impl ReadSessionGrants for StoreGrants {
    fn grants(
        &self,
        _: CancellationToken,
    ) -> BoxFuture<'_, Result<Vec<SessionGrant>, dim_agent_capabilities::host::HostError>> {
        Box::pin(async move {
            self.store
                .grants(&self.session_id)
                .await
                .map_err(|error| dim_agent_capabilities::host::HostError::failed(error.to_string()))
        })
    }
}

struct SessionGrantsPlugin {
    host: Key<SessionGrantsHost>,
    store: Key<SessionStore>,
    identity: Key<SessionIdentity>,
    config: Key<SessionConfig>,
    methods: Key<SessionMethods>,
}
impl SessionGrantsPlugin {
    fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            host: names.key()?,
            store: names.key()?,
            identity: names.key()?,
            config: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for SessionGrantsPlugin {
    fn name(&self) -> &str {
        "SessionGrants"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.store.erase(),
            self.identity.erase(),
            self.config.erase(),
            self.methods.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let store = ctx.require_at(&self.store)?;
            let identity = ctx.require_at(&self.identity)?;
            let config = ctx.require_at(&self.config)?;
            let methods = ctx.require_at(&self.methods)?;
            let session_id = identity.session_id.clone();
            let cwd = PathBuf::from(&config.cwd);
            ctx.provide_at(
                self.host.clone(),
                Arc::new(SessionGrantsHost(Arc::new(StoreGrants {
                    store: store.clone(),
                    session_id: session_id.clone(),
                }))),
            )?;
            let owner = store.clone();
            methods.register(
                ctx,
                "session.grants",
                ["session.grants.add", "session.grants.remove"]
                    .into_iter()
                    .map(|name| MethodDecl::new(name, true, Interruption::Report))
                    .collect(),
                Arc::new(move |method, value, _, _| {
                    let store = owner.clone();
                    let session_id = session_id.clone();
                    let cwd = cwd.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "session.grants.add" => {
                                let p: AddGrant = params(value)?;
                                let path = resolve(&cwd, p.path).await?;
                                store
                                    .add_grant(&session_id, path, p.kind, p.mode)
                                    .await
                                    .map_err(failed)?;
                                reply(serde_json::json!({}))
                            }
                            "session.grants.remove" => {
                                let p: RemoveGrant = params(value)?;
                                let path = resolve(&cwd, p.path).await?;
                                store
                                    .remove_grant(&session_id, path)
                                    .await
                                    .map_err(failed)?;
                                reply(serde_json::json!({}))
                            }
                            _ => unreachable!("registered session grants method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

fn failed(error: dim_agent_runtime::AgentError) -> RpcError {
    RpcError::app(-32000, "session_grants_failed", error.to_string())
}

/// Resolves a grant path against the Session workspace; an unresolvable path
/// is an invalid parameter, not a stored grants row.
async fn resolve(cwd: &std::path::Path, path: String) -> Result<PathBuf, RpcError> {
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || resolve_grant_path(&cwd, &path))
        .await
        .map_err(|error| {
            RpcError::app(
                -32000,
                "session_grants_failed",
                format!("grant path task failed: {error}"),
            )
        })?
        .map_err(RpcError::invalid)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddGrant {
    path: String,
    kind: GrantKind,
    mode: GrantMode,
}
#[derive(Deserialize)]
struct RemoveGrant {
    path: String,
}

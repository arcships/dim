//! The Session's prompt sources: the parts that provide one register it here
//! by name ([rutis usage R6](../../../docs/design/rutis-usage.md#r6-contributions-register-with-their-owner)).
//! It serves them as the `PromptHost` every branch's Prompt loads from, and
//! as the initial configuration that delivers all of them.
use crate::{
    host::{HostError, HostFuture, LoadPrompt, PromptHost},
    prompt::InitialPromptConfiguration,
};
use dim_agent_runtime::{ConfigureUpdate, Names, PromptSourceTarget, Registrations, SessionConfig};
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use rutis_bridge::session::{HostDispatch, Reference, Reply, Value, settle};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The prompt sources registered in one Session.
#[derive(Default)]
pub struct PromptRegistry(Arc<Registrations<Arc<dyn LoadPrompt>>>);
impl PromptRegistry {
    /// Registers the source `name` on behalf of the part that owns `ctx`,
    /// until that part unloads. A name registered already fails.
    pub fn register(
        &self,
        ctx: &Ctx,
        name: impl Into<String>,
        source: Arc<dyn LoadPrompt>,
    ) -> Result<(), Error> {
        self.0.register(ctx, name, source).map(|_| ())
    }
    /// The names registered now, in name order.
    pub fn names(&self) -> Vec<String> {
        self.0
            .all()
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }
}

impl LoadPrompt for PromptRegistry {
    fn load(
        &self,
        target: PromptSourceTarget,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        let Some(registration) = self.0.get(&target.source) else {
            return Box::pin(async move {
                Err(HostError::failed(format!(
                    "unknown prompt source: {}",
                    target.source
                )))
            });
        };
        Box::pin(async move { registration.value.load(target, cancel).await })
    }
}

struct ForeignPrompt(Reference);

impl LoadPrompt for ForeignPrompt {
    fn load(
        &self,
        target: PromptSourceTarget,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        Box::pin(async move {
            let cancel = cancel.child_token();
            let _finished = cancel.clone().drop_guard();
            let cancelled = Value::callback(move |_| {
                let cancel = cancel.clone();
                Ok(Value::future(async move {
                    cancel.cancelled().await;
                    Ok(Value::Undefined)
                }))
            });
            let reply = async {
                let target = rutis_bridge::session::arg(&target)?;
                let reply = self
                    .0
                    .call_async(Value::List(vec![target, cancelled]))
                    .await?;
                settle(reply).await?.json()
            }
            .await
            .map_err(|error| match &error {
                rutis_bridge::session::Error::Remote { message, .. }
                    if error.code() == Some("cancelled") =>
                {
                    HostError::Cancelled(message.clone())
                }
                rutis_bridge::session::Error::Remote { message, .. } => {
                    HostError::failed(message.clone())
                }
                _ => HostError::failed(error.to_string()),
            })?;
            serde_json::from_value(reply)
                .map_err(|error| HostError::failed(format!("invalid prompt entry: {error}")))
        })
    }
}

impl HostDispatch for PromptRegistry {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"names": "sync", "register": "async"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        let args = match args {
            Value::Undefined => vec![],
            args => args.list()?,
        };
        match method {
            "names" if args.is_empty() => rutis_bridge::session::arg(&self.names()),
            "register" => {
                let [name, Value::Reference(load)] = args.as_slice() else {
                    return Err(crate::port_faces::invalid(
                        "promptRegistry",
                        "register needs the source name and load function",
                    ));
                };
                if !load.is_function() {
                    return Err(crate::port_faces::invalid(
                        "promptRegistry",
                        "load must be a function",
                    ));
                }
                let name: String = serde_json::from_value(name.clone().json()?)
                    .map_err(|error| crate::port_faces::invalid("promptRegistry", error))?;
                let caller = rutis_bridge::session::caller_row().ok_or_else(|| {
                    crate::port_faces::invalid("promptRegistry", "register needs a calling row")
                })?;
                let result = self
                    .register(caller.ctx(), name, Arc::new(ForeignPrompt(load.clone())))
                    .map(|()| Value::Undefined)
                    .map_err(|error| {
                        rutis_bridge::exports::failure(
                            "PromptRegistrationError",
                            "failed",
                            error.to_string(),
                            None,
                        )
                    });
                Ok(Value::future(async move { result }))
            }
            "names" => Err(crate::port_faces::invalid(
                "promptRegistry",
                "names takes no arguments",
            )),
            _ => Err(crate::port_faces::unknown_method("promptRegistry", method)),
        }
    }
}

/// Every source applies to the root branch and the branches derived from it.
pub fn targets<'a>(sources: impl IntoIterator<Item = &'a str>) -> Vec<PromptSourceTarget> {
    sources
        .into_iter()
        .map(|source| PromptSourceTarget {
            source: source.into(),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
        })
        .collect()
}

pub struct PromptRegistryPlugin {
    config: Key<SessionConfig>,
    registry: Key<PromptRegistry>,
    registry_face: Key<dyn HostDispatch>,
    host: Key<PromptHost>,
    host_face: Key<dyn HostDispatch>,
    initial: Key<InitialPromptConfiguration>,
}
impl PromptRegistryPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            config: names.key()?,
            registry: names.key()?,
            registry_face: names.named("promptRegistry")?,
            host: names.key()?,
            host_face: names.named("promptHost")?,
            initial: names.key()?,
        })
    }
}
impl DynPlugin for PromptRegistryPlugin {
    fn name(&self) -> &str {
        "PromptRegistry"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.config.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let config = ctx.require_at(&self.config)?;
            let registry = Arc::new(PromptRegistry::default());
            let sources = registry.clone();
            let initial = InitialPromptConfiguration::read(move || {
                Some(ConfigureUpdate {
                    sources: targets(sources.names().iter().map(String::as_str)),
                    model: Some(config.model.clone()),
                })
            });
            let host = Arc::new(PromptHost(registry.clone()));
            ctx.provide_at(self.host.clone(), host.clone())?;
            ctx.provide_at(self.host_face.clone(), host as Arc<dyn HostDispatch>)?;
            ctx.provide_at(self.initial.clone(), Arc::new(initial))?;
            ctx.provide_at(self.registry.clone(), registry.clone())?;
            ctx.provide_at(
                self.registry_face.clone(),
                registry as Arc<dyn HostDispatch>,
            )?;
            Ok(Effect::Done)
        })
    }
}

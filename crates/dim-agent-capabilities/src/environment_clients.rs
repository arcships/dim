//! Session client presence as model state: who is attached, by channel.
//!
//! The App Server owns the connection and UI registries and provides the
//! read-only `SessionClientsHost`. This capability registers
//! `dim/environment/clients`; each branch adopts the current counts at every
//! turn start, so idle attach/detach churn does not become a block.
use crate::{
    host::{HostError, HostFuture, LoadPrompt},
    prompt_registry::{PromptRegistry, targets},
    prompt_sources::sha256_version,
};
use dim_agent_runtime::{
    AgentBuilder, ConfigureControl, ConfigureUpdate, Delivery, Dispatcher, Feature, Names,
    PromptSourceTarget, SessionIdentity,
};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The prompt source of the attached-client counts.
pub const CLIENTS: &str = "dim/environment/clients";

/// The channels the model may see. `tui` is a UI leaf; a leaf's underlying
/// connection is not counted again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClientChannel {
    Tui,
    Cli,
    Web,
    Acp,
    Remote,
    Desktop,
    Unknown,
}
impl ClientChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Cli => "cli",
            Self::Web => "web",
            Self::Acp => "acp",
            Self::Remote => "remote",
            Self::Desktop => "desktop",
            Self::Unknown => "unknown",
        }
    }
    /// The channel a `hello`-declared identity maps to, before the UI leaf
    /// check. A leaf's connection is `tui` and never reaches this mapping.
    pub fn declared(kind: DeclaredClientKind, name: Option<&str>) -> Self {
        match kind {
            DeclaredClientKind::Cli => Self::Cli,
            DeclaredClientKind::Web => Self::Web,
            DeclaredClientKind::Other => match name {
                Some("AcpGateway") => Self::Acp,
                Some("Desktop") | Some("dim-desktop") => Self::Desktop,
                _ => Self::Unknown,
            },
        }
    }
}

/// The protocol's `hello.clientKind`, reduced to what presence needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclaredClientKind {
    Cli,
    Web,
    Other,
}

/// Attached clients of one Session, by channel. Channels are sorted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientCounts {
    pub total: usize,
    pub by_channel: BTreeMap<&'static str, usize>,
}
impl ClientCounts {
    pub fn add(&mut self, channel: ClientChannel) {
        self.total += 1;
        *self.by_channel.entry(channel.as_str()).or_default() += 1;
    }
}

/// The App Server's read-only presence view.
pub trait SessionClientsSource: Send + Sync + 'static {
    /// The clients attached to `session_id` now. An unknown Session is empty.
    fn counts(&self, session_id: &str) -> ClientCounts;
}

/// The process port. The App Server provides it from its connection and UI
/// registries; an assembly without it does not install this feature.
pub struct SessionClientsHost(pub Arc<dyn SessionClientsSource>);

/// The feature's parts. The App Server daemon profile installs it after it
/// has provided `SessionClientsHost`.
pub fn feature() -> Feature {
    Feature::new("EnvironmentClients")
        .session("EnvironmentClients", |names| {
            Ok(Box::new(ClientsPlugin::new(names)?))
        })
        .branch("EnvironmentClientsBranch", |names| {
            Ok(Box::new(ClientsBranchPlugin::new(names)?))
        })
}

pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(feature());
}

/// The body: counts only, no labels, ids, times or departure history.
fn body(counts: &ClientCounts) -> String {
    if counts.total == 0 {
        return "No client is attached.".into();
    }
    let channels = counts
        .by_channel
        .iter()
        .map(|(channel, count)| format!("{count} {channel}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} {} attached: {channels}.",
        counts.total,
        if counts.total == 1 {
            "client"
        } else {
            "clients"
        }
    )
}

struct ClientsSource {
    host: Arc<SessionClientsHost>,
    session_id: String,
}
impl LoadPrompt for ClientsSource {
    fn load(
        &self,
        target: PromptSourceTarget,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled(
                    "client presence load cancelled".into(),
                ));
            }
            let body = body(&self.host.0.counts(&self.session_id));
            Ok(Some(PromptEntry {
                source: target.source,
                version: sha256_version(&body),
                body,
                scope: target.scope,
                delivery: PromptDelivery::State,
                order: 230,
            }))
        })
    }
}

struct ClientsPlugin {
    identity: Key<SessionIdentity>,
    clients: Key<SessionClientsHost>,
    prompts: Key<PromptRegistry>,
}
impl ClientsPlugin {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            identity: names.key()?,
            clients: names.key()?,
            prompts: names.key()?,
        })
    }
}
impl DynPlugin for ClientsPlugin {
    fn name(&self) -> &str {
        "EnvironmentClients"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.identity.erase(),
            self.clients.erase(),
            self.prompts.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let clients = ctx.require_at(&self.clients)?;
            let prompts = ctx.require_at(&self.prompts)?;
            prompts.register(
                ctx,
                CLIENTS,
                Arc::new(ClientsSource {
                    host: clients,
                    session_id: identity.session_id.clone(),
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

struct ClientsBranchPlugin {
    dispatcher: Key<Dispatcher>,
    configure: Key<dyn ConfigureControl>,
}
impl ClientsBranchPlugin {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            dispatcher: names.key()?,
            configure: names.key()?,
        })
    }
}
impl DynPlugin for ClientsBranchPlugin {
    fn name(&self) -> &str {
        "EnvironmentClientsBranch"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase(), self.configure.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let configure = ctx.require_at(&self.configure)?;
            let tasks = TaskTracker::new();
            let owned = tasks.clone();
            dispatcher
                .follow(
                    ctx,
                    "EnvironmentClients",
                    &["turn/start"],
                    move |delivery| {
                        if let Delivery::Live(_) = delivery {
                            let configure = configure.clone();
                            owned.spawn(async move {
                                let update = ConfigureUpdate {
                                    sources: targets([CLIENTS]),
                                    model: None,
                                };
                                if let Err(error) = configure.configure(update).await {
                                    tracing::warn!(%error, "client presence was not adopted");
                                }
                            });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_session_is_stated_as_such() {
        assert_eq!(body(&ClientCounts::default()), "No client is attached.");
    }

    #[test]
    fn counts_are_sorted_and_pluralized() {
        let mut counts = ClientCounts::default();
        counts.add(ClientChannel::Web);
        counts.add(ClientChannel::Tui);
        counts.add(ClientChannel::Tui);
        assert_eq!(body(&counts), "3 clients attached: 2 tui, 1 web.");
        let mut one = ClientCounts::default();
        one.add(ClientChannel::Tui);
        assert_eq!(body(&one), "1 client attached: 1 tui.");
    }

    #[test]
    fn a_declared_other_client_is_named_or_unknown() {
        assert_eq!(
            ClientChannel::declared(DeclaredClientKind::Other, Some("AcpGateway")),
            ClientChannel::Acp
        );
        assert_eq!(
            ClientChannel::declared(DeclaredClientKind::Other, Some("Desktop")),
            ClientChannel::Desktop
        );
        assert_eq!(
            ClientChannel::declared(DeclaredClientKind::Other, None),
            ClientChannel::Unknown
        );
        assert_eq!(
            ClientChannel::declared(DeclaredClientKind::Cli, None),
            ClientChannel::Cli
        );
    }
}

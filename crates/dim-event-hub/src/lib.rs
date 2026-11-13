//! Events from producers to subscribed Sessions, implemented natively. The
//! design is [docs/design/plugins/EventHub.md](../../../docs/design/plugins/EventHub.md).
//!
//! The process owner keeps the catalog of declared sources in memory, holds
//! the subscriptions in `eventhub/subscriptions.json`, and delivers each
//! published event to its target branches through Agents' `sessions.input`.
//! The Session part registers the guide entry that explains the markers and
//! the `events` tool.
mod bridge;
mod catalog;
mod error;
mod hub;
mod management;
mod model;
mod publish;
mod session;
mod state;
mod store;
mod subscribe;
mod subscriptions;
mod text;

use dim_agent_runtime::{AgentBuilder, Names, PartRegistry, PartScope};
use dim_app_server::plugin_files::PluginFileTransactions;
use dim_app_server::{ProcessActivity, methods::ManagementMethods};
use hub::Shared;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, host_key};
use std::sync::Arc;

pub use bridge::EventHub;
pub use catalog::{SourceReport, TypeReport};
pub use error::EventHubError;
pub use model::{
    DeclareRequest, ListRequest, PublishRequest, SubscribeRequest, SubscribeTarget, ToTarget,
    TypeDeclaration, UnsubscribeRequest,
};
pub use state::InstanceState;
pub use store::{GuideRecord, LastFailure, SUBSCRIPTIONS_FILE, Subscription};
pub use subscriptions::{EventSubscriptions, ListCall, SubscriptionCall, UnsubscribeCall};
pub use text::{ENTRY, EventHubTexts};

/// The process bus event that says the `eventHub` service is provided and
/// readable in this generation (design §2.1, §6). A producer registers its
/// listener first, then reads the service, so a row that applied before
/// EventHub — or while it was off — still learns when to declare.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Ready;
impl rutis::Event for Ready {
    const NAME: &'static str = "eventhub/ready";
    type Value = ();
}

/// Announces [`Ready`] once EventHub's provision is active (design §2.1).
///
/// A row can read another row's service only while its provider is active,
/// and EventHub's own fiber is loading until its `apply` returns, so a notice
/// emitted there would reach producers that then find no service. This row
/// injects `eventHub`, so rutis loads it only once the service is active; its
/// `apply` runs then, and every listener can read the service it announces.
struct ReadyAnnouncer {
    hub: Key<EventHub>,
}

impl DynPlugin for ReadyAnnouncer {
    fn name(&self) -> &str {
        "eventhub-ready"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.hub.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            // A listener's failure is its own, so it does not fail this row.
            if let Err(error) = ctx.parallel(Ready).await {
                tracing::warn!(%error, "EventHub could not announce that it is ready");
            }
            Ok(Effect::Done)
        })
    }
}

/// Installs EventHub as an optional feature: the process owner and the guide
/// entry. Neither is required, so a host without the App Server services it
/// needs (pluginFiles, managementMethods, sessions and partRegistry), or with
/// an unreadable subscription file, still starts and opens Sessions; only the
/// `eventHub` name, `eventhub.*` and the entry are unavailable.
pub fn install(builder: &mut AgentBuilder) {
    install_with(builder, EventHubTexts::default());
}

/// [`install`] with `texts` in place of the shipped texts: a candidate
/// replaces the guide entry and the tool description at the factory, so the
/// code keeps no second copy (design A.4, A.6, model-prompts §8.1).
pub fn install_with(builder: &mut AgentBuilder, texts: EventHubTexts) {
    dim_agent_runtime::names::register::<EventHub>(builder.catalog_mut(), None);
    builder
        .catalog_mut()
        .register_shared(dim_agent_runtime::names::name_of::<EventSubscriptions>());
    // Producers of every language read the service (design Appendix F): the
    // shared name lets a row of another runtime be leased the dispatcher this
    // part provides at `host_key("eventHub")`.
    builder
        .catalog_mut()
        .register_shared(dim_agent_runtime::names::name_of::<EventHub>());
    let texts = Arc::new(texts);
    // One state for the install (design §2.1): the factory captures it, so the
    // catalog, the locks and the budget survive every reapply and every off
    // and on of the row, and end with the process.
    let state = Arc::new(InstanceState::default());
    builder.optional_plugin(move |names| {
        Ok(
            Box::new(EventHubPlugin::new(names, texts.clone(), state.clone())?)
                as Box<dyn DynPlugin>,
        )
    });
}

pub struct EventHubPlugin {
    files: Key<PluginFileTransactions>,
    methods: Key<ManagementMethods>,
    sessions: Key<dyn HostDispatch>,
    parts: Key<PartRegistry>,
    hub: Key<EventHub>,
    activity: Key<ProcessActivity>,
    /// The install's state, captured by the factory (design §2.1): nothing
    /// install-lifetime is created in `new` or `apply`.
    state: Arc<InstanceState>,
    /// The texts the Session part registers (design A.4, A.6), from the row's
    /// factory configuration.
    texts: Arc<EventHubTexts>,
}

impl EventHubPlugin {
    pub fn new(
        names: &Names<'_>,
        texts: Arc<EventHubTexts>,
        state: Arc<InstanceState>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            files: names.key()?,
            methods: names.key()?,
            sessions: host_key("sessions"),
            parts: names.key()?,
            hub: names.key()?,
            activity: names.key()?,
            state,
            texts,
        })
    }
}

impl DynPlugin for EventHubPlugin {
    fn name(&self) -> &str {
        "eventhub"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.files.erase(),
            self.methods.erase(),
            self.sessions.erase(),
            self.parts.erase(),
            self.activity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let files = ctx.require_at(&self.files)?;
            let sessions = ctx.require_at(&self.sessions)?;
            let store = Arc::new(store::Store::new(files, self.state.clone()));
            // The file is read once, into the install's view; a corrupt one
            // fails this part and leaves its bytes unchanged (design A.5).
            store.reload().await.map_err(|error| {
                rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    format!("event hub subscriptions: {error}").into(),
                ))
            })?;
            let shared = Arc::new(Shared {
                ctx: ctx.clone(),
                activity: ctx.require_at(&self.activity)?,
                sessions: sessions.clone(),
                state: self.state.clone(),
                store,
                stop: ctx.cancellation_token(),
            });
            ctx.provide_as::<EventHub>(Arc::new(EventHub {
                shared: shared.clone(),
            }))?;
            let subscriptions = Arc::new(EventSubscriptions::new(shared.clone()));
            ctx.provide_as::<EventSubscriptions>(subscriptions.clone())?;
            ctx.provide_at::<dyn HostDispatch>(
                host_key("eventSubscriptions"),
                Arc::new(bridge::ForeignEventSubscriptions {
                    service: subscriptions,
                }),
            )?;
            ctx.provide_at::<dyn HostDispatch>(
                host_key("eventHub"),
                Arc::new(bridge::ForeignEventHub {
                    shared: shared.clone(),
                }),
            )?;
            {
                let shared = shared.clone();
                let reconciliation = tokio::spawn(publish::reconcile(shared));
                ctx.effect(move || async move {
                    reconciliation.await.map_err(|error| {
                        rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                            error.to_string().into(),
                        ))
                    })?;
                    Ok(())
                })?;
            }
            ctx.require_at(&self.methods)?.register(
                ctx,
                management::NAMESPACE,
                management::declarations(),
                management::handler(shared.clone()),
            )?;
            // A deleted Session's subscriptions are removed through the
            // process notice (design §3.4).
            {
                let shared = shared.clone();
                ctx.listen::<dim_app_server::SessionDeleted>()
                    .on(move |event, _| {
                        let shared = shared.clone();
                        let session_id = event.session_id.clone();
                        async move {
                            shared.session_deleted(&session_id).await;
                            Ok(None)
                        }
                    })?;
            }
            let texts = self.texts.clone();
            ctx.require_at(&self.parts)?.register(
                ctx,
                PartScope::Session,
                "EventHubSession",
                move |mount| {
                    Ok(Some(Box::new(crate::session::EventHubSession::new(
                        mount.names,
                        texts.clone(),
                    )?)))
                },
            )?;
            // Producers learn here that the service exists (design §2.1). The
            // notice comes from a dependent row: this fiber is loading until
            // `apply` returns, and another row reads a service only while its
            // provider is active, so an emit here would reach producers that
            // find no service. The dependent loads the moment `eventHub` is
            // active and announces it.
            ctx.plugin(ReadyAnnouncer {
                hub: self.hub.clone(),
            });
            Ok(Effect::Done)
        })
    }
}

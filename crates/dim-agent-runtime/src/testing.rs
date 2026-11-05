//! A minimal host for tests, examples and tools.
//!
//! A host has one loader, and an Agent is built in it ([`AgentBuilder::build`]).
//! The App Server is the product's host. [`MinimalHost`] is the same assembly
//! with nothing else: one rutis root, one loader with the builder's modules
//! and plugin rows, the builder's [`AGENT_GROUP`] row and one Agent instance.
//! It owns the root's lifetime.

use crate::assembly::{Factory, loader_error, row};
use crate::*;
use rutis::host::{BoxFuture as RutisFuture, CtxHost, DynPlugin, Effect, ErrorKind};
use rutis::{Ctx, Error};
use rutis_loader::Loader;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use tokio_util::sync::CancellationToken;

/// One root, its loader and one Agent built in it.
///
/// It dereferences to its [`Agent`]. [`MinimalHost::close`] closes the
/// Agent's Sessions and then shuts the root down; dropping the host does the
/// same in the background.
pub struct MinimalHost {
    agent: Agent,
    root: Ctx,
    loader: Loader,
    /// The assembly's rows, the first layer of every reconcile.
    rows: Value,
    closed: bool,
}

impl MinimalHost {
    /// Assembles a host for `builder` and builds its Agent: the builder's
    /// plugins as process rows, its modules, its [`AGENT_GROUP`] row and one
    /// Agent instance. The host's part registry is its own, so two hosts built
    /// from one builder mount only their own plugins' parts.
    pub async fn start(builder: &AgentBuilder) -> Result<Self> {
        Self::start_with(builder, |_, _| Ok(())).await
    }

    /// [`Self::start`], with `prepare` run on the new root before the loader
    /// mounts: it may provide host values there and register modules of the
    /// caller's own, for rows it adds later with [`Self::set_rows`]. Host
    /// plugins are rows ([`AgentBuilder::plugin`]), not children of the root.
    pub async fn start_with(
        builder: &AgentBuilder,
        prepare: impl FnOnce(&Ctx, &mut Modules) -> Result<()>,
    ) -> Result<Self> {
        let root = Ctx::root_with_sink(
            tokio::runtime::Handle::current(),
            Arc::new(|error| tracing::error!(%error, "Agent runtime error")),
        );
        match assemble(builder, &root, prepare).await {
            Ok((agent, loader, rows)) => Ok(Self {
                agent,
                root,
                loader,
                rows,
                closed: false,
            }),
            Err(error) => Err(match root.shutdown().await {
                Ok(()) => error,
                Err(cleanup) => error.with_cleanup(AgentError::from(cleanup)),
            }),
        }
    }

    /// The Agent built in this host.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// The host's root context.
    pub fn root(&self) -> &Ctx {
        &self.root
    }

    /// The host's loader.
    pub fn loader(&self) -> &Loader {
        &self.loader
    }

    /// Replaces the caller's rows, a layer reconciled after the assembly's:
    /// `rows` are loader rows whose modules [`Self::start_with`] registered.
    /// An empty list removes them.
    pub async fn set_rows(&self, rows: Vec<Value>) -> Result<()> {
        let layers = [
            ("assembly", self.rows.clone()),
            ("host", json!([{ "insert": rows }])),
        ]
        .into_iter()
        .map(|(name, rows)| {
            serde_json::from_value(rows)
                .map(|rows| rutis_loader::Layer::new(name, rows))
                .map_err(|error| AgentError::Build(error.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;
        let report = self
            .loader
            .reconcile(layers, None)
            .await
            .map_err(loader_error)?;
        if !report.failures.is_empty() || !report.issues.is_empty() {
            return Err(AgentError::Build(format!(
                "{:?} {:?}",
                report.failures, report.issues
            )));
        }
        Ok(())
    }

    /// Closes the Agent's Sessions, then shuts the root down, which unloads
    /// the Agent instance and the host's rows.
    pub async fn close(mut self) -> Result<()> {
        self.closed = true;
        let mut errors = vec![];
        errors.extend(self.agent.close().await.err());
        errors.extend(self.root.shutdown().await.err().map(AgentError::from));
        AgentError::aggregate(errors)
    }
}

impl std::ops::Deref for MinimalHost {
    type Target = Agent;
    fn deref(&self) -> &Agent {
        &self.agent
    }
}

impl Drop for MinimalHost {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let (close, root) = (self.agent.close(), self.root.clone());
        self.root.handle().clone().spawn(async move {
            let _ = close.await;
            let _ = root.shutdown().await;
        });
    }
}

/// Mounts the loader on `root`, reconciles the assembly and creates the
/// Agent instance, then builds the Agent in it.
async fn assemble(
    builder: &AgentBuilder,
    root: &Ctx,
    prepare: impl FnOnce(&Ctx, &mut Modules) -> Result<()>,
) -> Result<(Agent, Loader, Value)> {
    let builder = builder.with_own_registry();
    let mut builtins = Modules::new();
    builtins.register("AgentAnchor", |build| {
        let slot = build.value::<AnchorSlot>().ok_or_else(|| {
            rutis::Error::from(ErrorKind::PluginFailed(
                "Agent created without its slot".into(),
            ))
        })?;
        Factory::new(move || Box::new(AgentAnchor((*slot).clone())) as Box<dyn DynPlugin>)
    });
    prepare(root, &mut builtins)?;
    let mut rows = match builder.has_plugins() {
        true => builder.plugin_rows(&mut builtins).await?.rows,
        false => vec![],
    };
    let session_row = builder.install(&mut builtins, []).await?;
    rows.push(builder.agent_row([row("AgentAnchor"), session_row]));
    let plugin = rutis_loader::LoaderPlugin::new(
        builtins,
        rutis_loader::LoaderOptions {
            catalog: builder.catalog().clone(),
            ..Default::default()
        },
    );
    let loader = plugin.handle();
    let view = root.plugin(plugin);
    let cancel = CancellationToken::new();
    wait_active(
        root,
        std::slice::from_ref(&view),
        builder.opening_timeout(),
        &cancel,
    )
    .await?;
    let assembly = json!([{ "insert": rows }]);
    let layer = serde_json::from_value(assembly.clone())
        .map_err(|error| AgentError::Build(error.to_string()))?;
    let report = loader
        .reconcile(vec![rutis_loader::Layer::new("assembly", layer)], None)
        .await
        .map_err(loader_error)?;
    if !report.failures.is_empty() || !report.issues.is_empty() {
        return Err(AgentError::Build(format!(
            "{:?} {:?}",
            report.failures, report.issues
        )));
    }
    let slot = AnchorSlot::default();
    loader
        .create_instance(root, AGENT_GROUP)
        .with(slot.clone())
        .await
        .map_err(loader_error)?;
    let anchor = slot
        .0
        .get()
        .cloned()
        .ok_or_else(|| AgentError::Build("Agent anchor did not load".into()))?;
    let agent = builder.build(loader.clone(), &anchor);
    Ok((agent, loader, assembly))
}

#[derive(Clone, Default)]
struct AnchorSlot(Arc<OnceLock<Ctx>>);

/// The plugin of the minimal host's Agent instance: the Agent's Sessions are
/// created from its context.
struct AgentAnchor(AnchorSlot);
impl DynPlugin for AgentAnchor {
    fn name(&self) -> &str {
        "AgentAnchor"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> RutisFuture<'a, std::result::Result<Effect, Error>> {
        Box::pin(async move {
            self.0.0.set(ctx.clone()).map_err(|_| {
                rutis::Error::from(ErrorKind::PluginFailed("Agent anchor reloaded".into()))
            })?;
            Ok(Effect::Done)
        })
    }
}

/// How many facts one read of a log asks for.
const EVENTS_PAGE: u64 = 4096;

/// The facts a flow reads from one log: its own branch through the branch
/// query, or the Timeline service itself.
#[derive(Clone, Copy)]
enum Log<'a> {
    Session(&'a Session, &'a str),
    Timeline(&'a Timeline),
}
impl Log<'_> {
    async fn query(&self, query: TimelineQuery) -> Result<TimelineQueryReply> {
        match self {
            Self::Session(session, branch) => session.query(branch, query).await,
            Self::Timeline(timeline) => timeline.query(query).await,
        }
    }
    fn branch(&self) -> String {
        match self {
            Self::Session(_, branch) => (*branch).to_owned(),
            Self::Timeline(timeline) => timeline.branch_id(),
        }
    }
}

/// The facts `session` published on `branch` alone within `range`, oldest
/// first: the whole selection, read a page at a time through the branch's
/// query. A flow reads a branch's log with this instead of paging by hand.
pub async fn branch_events<R: std::ops::RangeBounds<u64> + Send>(
    session: &Session,
    branch: &str,
    range: R,
) -> Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
    read(
        Log::Session(session, branch),
        TimelineQueryScope::Branch,
        range,
        EVENTS_PAGE,
    )
    .await
}

/// [`branch_events`] over the branch's chain: each ancestor to its cut and then
/// the branch, root first.
pub async fn chain_events<R: std::ops::RangeBounds<u64> + Send>(
    session: &Session,
    branch: &str,
    range: R,
) -> Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
    read(
        Log::Session(session, branch),
        TimelineQueryScope::Chain,
        range,
        EVENTS_PAGE,
    )
    .await
}

/// [`chain_events`] with an explicit page size, so a flow can cross the
/// ancestor-to-branch boundary of a chain read without a log of thousands of
/// facts.
pub async fn chain_events_paged<R: std::ops::RangeBounds<u64> + Send>(
    session: &Session,
    branch: &str,
    range: R,
    page: u64,
) -> Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
    read(
        Log::Session(session, branch),
        TimelineQueryScope::Chain,
        range,
        page,
    )
    .await
}

/// [`branch_events`] through the Timeline service: every fact of the queried
/// branch or of its chain within `range`, including the branch's own facts
/// written but not yet published.
pub async fn timeline_events<R: std::ops::RangeBounds<u64> + Send>(
    timeline: &Timeline,
    scope: TimelineQueryScope,
    range: R,
) -> Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
    read(Log::Timeline(timeline), scope, range, EVENTS_PAGE).await
}

async fn read<R: std::ops::RangeBounds<u64> + Send>(
    log: Log<'_>,
    scope: TimelineQueryScope,
    range: R,
    page: u64,
) -> Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
    use std::ops::Bound;
    let start = match range.start_bound() {
        Bound::Included(start) => *start,
        Bound::Excluded(start) => start.saturating_add(1),
        Bound::Unbounded => 1,
    }
    .max(1);
    let end = match range.end_bound() {
        Bound::Included(end) => Some(*end),
        Bound::Excluded(end) => Some(end.saturating_sub(1)),
        Bound::Unbounded => None,
    };
    let mut query = TimelineQuery {
        scope,
        from_event: None,
        from_sequence: (start > 1).then_some(start),
        order_by: TimelineQueryOrderBy::Sequence,
        direction: TimelineQueryDirection::Ascending,
        include_publishing: matches!(log, Log::Timeline(_)),
        kinds: Vec::new(),
        flags: Vec::new(),
        event_id: None,
        predicate: None,
        predicates: Vec::new(),
        limit: std::num::NonZeroU64::new(page).expect("a page keeps at least one fact"),
        offset: 0,
    };
    let own = log.branch();
    let mut events: Vec<(
        dim_agent_types::TimelinePosition,
        Arc<dim_agent_types::AgentEvent>,
    )> = Vec::new();
    // The fact the last page ended at, when the next page begins there again:
    // a chain read anchored by event id includes the fact it names, so that
    // page asks for one fact more than it keeps.
    let mut repeated = None;
    let mut request = page;
    loop {
        let mut reply = log.query(query.clone()).await?;
        let short = (reply.events.len() as u64) < request;
        if repeated.is_some() && repeated.as_ref() == reply.positions.first() {
            reply.events.remove(0);
            reply.positions.remove(0);
        }
        let last = reply.positions.last().cloned();
        events.extend(reply.positions.into_iter().zip(reply.events));
        if short {
            break;
        }
        match last {
            // A chain read's `from_sequence` is anchored on the queried branch,
            // so a page that ended in an ancestor continues at that fact's id.
            Some(last) if last.branch_id != own => {
                let Some(id) = events.last().map(|(_, event)| event.id.clone()) else {
                    break;
                };
                query.from_sequence = None;
                query.from_event = Some(id);
                request = page + 1;
                query.limit =
                    std::num::NonZeroU64::new(request).expect("a page keeps at least one fact");
                repeated = Some(last);
            }
            Some(last) => {
                query.from_event = None;
                query.from_sequence = Some(last.sequence + 1);
                request = page;
                query.limit =
                    std::num::NonZeroU64::new(request).expect("a page keeps at least one fact");
                repeated = None;
            }
            None => break,
        }
        if end.is_some_and(|end| {
            events.last().is_some_and(|(position, _)| {
                // The end bounds the queried branch: a page that ends on an
                // ancestor continues, since its sequence says nothing about
                // the branch reaching the end.
                position.branch_id == own && position.sequence >= end
            })
        }) {
            break;
        }
    }
    Ok(events
        .into_iter()
        .filter(|(position, event)| match end {
            // The end bounds the branch the range names; an ancestor is read
            // to its cut.
            Some(end) => position.branch_id != own || event.sequence <= end,
            None => true,
        })
        .map(|(_, event)| event)
        .collect())
}

use super::keys::KeyFilter;
use crate::{AgentError, BoxFuture, Result, Timeline, panic_message};
use dim_agent_types::AgentEvent;
use futures_util::FutureExt;
use rutis::{CordisError, Ctx, Event, EventKey, InstanceId, Listener};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// The committed timeline event delivered through the rutis event bus.
pub(crate) struct Committed(pub Arc<AgentEvent>);
impl Event for Committed {
    const NAME: &'static str = "timeline.committed";
    type Value = ();
}

type Handler = dyn Fn(Arc<AgentEvent>) -> BoxFuture<'static, Result<()>> + Send + Sync;

/// Forwards committed events to one subscriber: unmatched keys are skipped,
/// handler errors become an explicit dispatcher failure, and handler panics
/// are attributed to the subscriber before the defect stops the branch.
struct SubscriptionListener {
    name: String,
    keys: KeyFilter,
    handler: Arc<Handler>,
    failures: Dispatcher,
}
impl Listener<Committed> for SubscriptionListener {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        event: &'a Committed,
    ) -> BoxFuture<'a, std::result::Result<Option<()>, CordisError>> {
        if !self.keys.matches(event.0.kind.key()) {
            return Box::pin(async { Ok(None) });
        }
        let name = self.name.clone();
        let handler = self.handler.clone();
        let failures = self.failures.clone();
        let event = event.0.clone();
        Box::pin(async move {
            let future = match catch_unwind(AssertUnwindSafe(|| handler(event))) {
                Ok(future) => future,
                Err(panic) => return Err(self::panic(&failures, &name, panic)),
            };
            match AssertUnwindSafe(future).catch_unwind().await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.fail(error),
                Err(panic) => return Err(self::panic(&failures, &name, panic)),
            }
            Ok(None)
        })
    }
}

/// Records an attributed fault and returns the defect that stops dispatch.
fn panic(failures: &Dispatcher, name: &str, payload: Box<dyn std::any::Any + Send>) -> CordisError {
    failures.fail(AgentError::fault(name, panic_message(payload)));
    CordisError::PluginFailed(format!("{name} handler panicked").into())
}

#[derive(Clone, Debug, Default)]
pub struct DispatchState {
    pub sequence: u64,
    pub failure: Option<AgentError>,
}
/// Delivers one branch's committed events on that branch's instance channel.
#[derive(Clone)]
pub struct Dispatcher {
    state: Arc<watch::Sender<DispatchState>>,
    instance: InstanceId,
    timeline: Timeline,
}
impl Dispatcher {
    pub(crate) fn new(instance: InstanceId, sequence: u64, timeline: Timeline) -> Self {
        Self {
            state: Arc::new(
                watch::channel(DispatchState {
                    sequence,
                    failure: None,
                })
                .0,
            ),
            instance,
            timeline,
        }
    }
    /// Registers a bus listener under `name` for the given event keys: exact
    /// keys, `"*"` for every key, or `"domain/*"` for every key in a domain.
    ///
    /// Handler errors are infrastructure failures: they surface as an
    /// explicit dispatcher failure instead of propagating through dispatch.
    /// Business outcomes are written to the Timeline by the handler itself.
    /// Handler panics are attributed to `name` and stop the branch.
    ///
    /// `ctx` must be inside the branch subtree; the listener lives on the
    /// branch instance channel and is removed with the registering plugin.
    pub fn subscribe(
        &self,
        ctx: &Ctx,
        name: impl Into<String>,
        keys: &[&str],
        handler: impl Fn(Arc<AgentEvent>) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static,
    ) -> Result<()> {
        let keys = KeyFilter::new(keys)?;
        let handler: Arc<Handler> = Arc::new(handler);
        ctx.events()
            .on(
                ctx,
                &EventKey::<Committed>::of().instance(self.instance),
                SubscriptionListener {
                    name: name.into(),
                    keys,
                    handler,
                    failures: self.clone(),
                },
            )
            .map_err(AgentError::from)?;
        Ok(())
    }
    /// Registers a bus listener for every committed event of this branch;
    /// same failure and lifetime rules as [`Self::subscribe`].
    pub fn subscribe_all(
        &self,
        ctx: &Ctx,
        name: impl Into<String>,
        handler: impl Fn(Arc<AgentEvent>) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static,
    ) -> Result<()> {
        self.subscribe(ctx, name, &["*"], handler)
    }
    /// [`Self::subscribe`] in data form, for parts in other languages: the
    /// calling row's `ctx`, owned keys, and a handler taking the committed
    /// event by value whose failure is the subscriber's handler failure.
    pub fn listen(
        &self,
        ctx: &Ctx,
        name: String,
        keys: Vec<String>,
        handler: Arc<
            dyn Fn(AgentEvent) -> BoxFuture<'static, std::result::Result<(), CordisError>>
                + Send
                + Sync,
        >,
    ) -> Result<()> {
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        self.subscribe(ctx, name, &keys, move |event| {
            let handler = handler.clone();
            Box::pin(async move { handler((*event).clone()).await.map_err(AgentError::from) })
        })
    }
    /// The branch's committed sequence: a subscriber's rebuild cutoff. It
    /// rebuilds from [`Self::local`] through it and skips live events at or
    /// below it.
    pub fn committed(&self) -> u64 {
        self.timeline.committed()
    }
    // The exported reads spell out `std::result::Result`: rutis' export
    // generator resolves an alias's item type where the alias is defined
    // (arcships/rutis#463).
    /// This branch's own committed events in `start..=through`.
    pub async fn local(
        &self,
        start: u64,
        through: u64,
    ) -> std::result::Result<Vec<Arc<AgentEvent>>, AgentError> {
        self.timeline.local(start..=through).await
    }
    /// The branch's committed events through `through`, ancestor prefix included.
    pub async fn history(
        &self,
        through: u64,
    ) -> std::result::Result<Vec<Arc<AgentEvent>>, AgentError> {
        self.timeline.history(Some(through)).await
    }
    pub fn notify(&self) {
        self.state.send_modify(|_| {});
    }
    pub fn fail(&self, error: AgentError) {
        self.state.send_modify(|s| {
            if s.failure.is_none() {
                s.failure = Some(error);
            }
        });
    }
    pub fn sequence(&self) -> u64 {
        self.state.borrow().sequence
    }
    pub fn failure(&self) -> Option<AgentError> {
        self.state.borrow().failure.clone()
    }
    pub fn watch(&self) -> watch::Receiver<DispatchState> {
        self.state.subscribe()
    }
    pub async fn changed(&self) {
        let _ = self.watch().changed().await;
    }
    pub(crate) async fn run(&self, timeline: Timeline, ctx: Ctx, stop: CancellationToken) {
        let mut reader = timeline.read(timeline.restored_sequence());
        let mut changed = self.watch();
        loop {
            if self.failure().is_some() {
                return;
            }
            let event = tokio::select! {
                _=stop.cancelled()=>return,
                _=changed.changed()=>continue,
                value=reader.next()=>match value{Ok(Some(e))=>e,Ok(None)=>return,Err(error)=>{self.fail(error);return;}},
            };
            if let Err(error) = ctx
                .events()
                .serial(
                    &ctx,
                    &EventKey::of().instance(self.instance),
                    &Committed(event.clone()),
                )
                .await
            {
                self.fail(AgentError::from(error));
                return;
            }
            timeline.dispatched(event.sequence);
            self.state.send_modify(|s| s.sequence = event.sequence);
        }
    }
}

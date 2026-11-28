//! Broadcasts every dispatched event of one branch on its Session channel,
//! which also carries owners' notices about the Session's process-level state.
use dim_agent_runtime::{BranchLifecycle, Dispatcher, Names, SessionLifecycle, SessionScope};
use dim_agent_types::AgentEvent;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Event};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A record on one Session's channel; the channel name identifies the Session.
pub enum SessionBroadcastEvent {
    /// One event dispatched by its branch.
    Event {
        branch_id: String,
        event: Arc<AgentEvent>,
    },
    /// A committed change of process-level state about this Session.
    Notice(Arc<SessionNotice>),
    /// A host observation fence. Tokens have no Agent or wire semantics.
    Fence { recipient: String, token: u64 },
    /// The last event record: every branch has stopped dispatching. Host
    /// fences and notices racing the close may follow; listeners can settle
    /// fences from this end record.
    Closed,
}
/// A record sent on one Session's channel ([`session_channel`]). It
/// serializes as its record: the channel is the name it is sent under.
pub struct SessionBroadcast {
    channel: String,
    pub record: SessionBroadcastEvent,
}
impl SessionBroadcast {
    pub fn new(session_id: &str, record: SessionBroadcastEvent) -> Self {
        Self {
            channel: session_channel(session_id),
            record,
        }
    }
}
impl Event for SessionBroadcast {
    const NAME: &'static str = "";
    type Value = ();
    fn name(&self) -> &str {
        &self.channel
    }
}
impl Serialize for SessionBroadcast {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.record.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for SessionBroadcast {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            channel: String::new(),
            record: SessionBroadcastEvent::deserialize(deserializer)?,
        })
    }
}

/// The public serde form of a [`SessionBroadcastEvent`], which every runtime
/// listening to `session/{id}` receives: `{kind: "event", branchId, event}`,
/// `{kind: "notice", notice}`, `{kind: "fence", recipient, token}` or
/// `{kind: "closed"}`. A notice keeps its own `kind` field inside `notice`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum WireRef<'a> {
    Event {
        #[serde(rename = "branchId")]
        branch_id: &'a str,
        event: &'a AgentEvent,
    },
    Notice {
        notice: &'a SessionNotice,
    },
    Fence {
        recipient: &'a str,
        token: u64,
    },
    Closed,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Wire {
    Event {
        #[serde(rename = "branchId")]
        branch_id: String,
        event: Arc<AgentEvent>,
    },
    Notice {
        notice: Arc<SessionNotice>,
    },
    Fence {
        recipient: String,
        token: u64,
    },
    Closed,
}
impl Serialize for SessionBroadcastEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Event { branch_id, event } => WireRef::Event { branch_id, event },
            Self::Notice(notice) => WireRef::Notice { notice },
            Self::Fence { recipient, token } => WireRef::Fence {
                recipient,
                token: *token,
            },
            Self::Closed => WireRef::Closed,
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for SessionBroadcastEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Event { branch_id, event } => Self::Event { branch_id, event },
            Wire::Notice { notice } => Self::Notice(notice),
            Wire::Fence { recipient, token } => Self::Fence { recipient, token },
            Wire::Closed => Self::Closed,
        })
    }
}

/// The rutis channel shared by the broadcaster and every listener.
pub fn session_channel(session_id: &str) -> String {
    format!("session/{session_id}")
}

/// An owner's announcement that its process-level state about a Session
/// changed. The broadcast and Rpc never interpret the fields; it has no
/// sequence and is never replayed.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionNotice {
    /// The owner, such as `SessionStore`.
    pub source: String,
    /// The owner's kind of change, such as `title`.
    pub kind: String,
    /// The owner's data for that kind.
    pub data: serde_json::Value,
}

/// Announces a committed change on the Session's channel, from the owner's
/// own context after the commit. It does not wait for listeners: without one
/// (the Session is not loaded or not observed) the notice is dropped.
pub fn notify(ctx: &Ctx, session_id: &str, notice: SessionNotice) {
    let notice = SessionBroadcastEvent::Notice(Arc::new(notice));
    if let Err(error) = ctx.emit(SessionBroadcast::new(session_id, notice)) {
        tracing::warn!(%error, session_id, "failed to emit a Session notice");
    }
}

pub struct SessionBroadcastPlugin {
    dispatcher: Key<Dispatcher>,
    lifecycle: Key<BranchLifecycle>,
}
impl SessionBroadcastPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            dispatcher: names.key()?,
            lifecycle: names.key()?,
        })
    }
}
impl DynPlugin for SessionBroadcastPlugin {
    fn name(&self) -> &str {
        "SessionBroadcast"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase(), self.lifecycle.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let session_id = lifecycle.session_id();
            let branch_id = lifecycle.branch_id().clone();
            let emitter = ctx.clone();
            dispatcher.subscribe_all(ctx, "SessionBroadcast", move |event| {
                let record = SessionBroadcastEvent::Event {
                    branch_id: branch_id.clone(),
                    event,
                };
                let _ = emitter.emit(SessionBroadcast::new(&session_id, record));
                Box::pin(async { Ok(()) })
            })?;
            Ok(Effect::Done)
        })
    }
}
/// Ends the Session channel. Session capabilities unload after every branch
/// closed, so this record follows every broadcast event.
pub struct SessionBroadcastEndPlugin {
    identity: Key<SessionScope>,
    lifecycle: Key<SessionLifecycle>,
}
impl SessionBroadcastEndPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            identity: names.key()?,
            lifecycle: names.key()?,
        })
    }
}
impl DynPlugin for SessionBroadcastEndPlugin {
    fn name(&self) -> &str {
        "SessionBroadcastEnd"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase(), self.lifecycle.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let emitter = ctx.clone();
            ctx.effect(move || async move {
                if !lifecycle.unloading() {
                    let record = SessionBroadcastEvent::Closed;
                    let _ = emitter.emit(SessionBroadcast::new(&identity.session_id(), record));
                }
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

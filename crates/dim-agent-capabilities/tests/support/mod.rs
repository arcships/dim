//! Follows one branch the way a host does: listen on the Session broadcast
//! first, then read the Timeline history and drop what the broadcast repeats.
use dim_agent_capabilities::session_broadcast::{
    SessionBroadcast, SessionBroadcastEvent, session_channel,
};
use dim_agent_runtime::Session;
use dim_agent_runtime::testing::branch_events;
use dim_agent_types::AgentEvent;
use rutis::Ctx;
use rutis::Disposer;
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::mpsc;

pub struct Events {
    history: VecDeque<Arc<AgentEvent>>,
    live: mpsc::UnboundedReceiver<Option<Arc<AgentEvent>>>,
    last: u64,
    ended: bool,
    _listener: Disposer,
}
impl Events {
    /// Events of `branch` after `after`, then each one as it is dispatched.
    pub async fn follow(ctx: &Ctx, session: &Session, branch: &str, after: u64) -> Self {
        let (sender, live) = mpsc::unbounded_channel();
        let branch = branch.to_owned();
        let listener = ctx
            .listen::<SessionBroadcast>()
            .name(session_channel(session.id()))
            .on({
                let branch = branch.clone();
                move |event, _| {
                    match &event.record {
                        SessionBroadcastEvent::Event { branch_id, event }
                            if *branch_id == branch =>
                        {
                            let _ = sender.send(Some(event.clone()));
                        }
                        SessionBroadcastEvent::Event { .. } => {}
                        SessionBroadcastEvent::Notice(_) | SessionBroadcastEvent::Fence { .. } => {}
                        SessionBroadcastEvent::Closed => {
                            let _ = sender.send(None);
                        }
                    }
                    async { Ok(None) }
                }
            })
            .unwrap();
        let history: VecDeque<_> = branch_events(session, &branch, ..)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.sequence > after)
            .collect();
        Self {
            history,
            live,
            last: after,
            ended: false,
            _listener: listener,
        }
    }
    /// `None` once the Session channel has ended.
    pub async fn next(&mut self) -> Option<Arc<AgentEvent>> {
        loop {
            let event = match self.history.pop_front() {
                Some(event) => event,
                None if self.ended => return None,
                None => match self.live.recv().await.flatten() {
                    Some(event) => event,
                    None => {
                        self.ended = true;
                        return None;
                    }
                },
            };
            if event.sequence > self.last {
                self.last = event.sequence;
                return Some(event);
            }
        }
    }
}

/// Reads committed events while dispatch may still be held by a test handler.
pub struct Committed {
    session: Session,
    branch: String,
    last: u64,
}
impl Committed {
    pub fn after(session: &Session, branch: &str, after: u64) -> Self {
        Self {
            session: session.clone(),
            branch: branch.to_owned(),
            last: after,
        }
    }
    pub async fn next(&mut self) -> Arc<AgentEvent> {
        loop {
            let next = branch_events(&self.session, &self.branch, ..)
                .await
                .unwrap()
                .into_iter()
                .find(|event| event.sequence > self.last);
            if let Some(event) = next {
                self.last = event.sequence;
                return event;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }
}

/// Every record of one Session channel; `None` is the end record.
pub type Record = Option<(String, Arc<AgentEvent>)>;
pub struct Broadcast {
    records: Arc<std::sync::Mutex<Vec<Record>>>,
    changed: Arc<tokio::sync::Notify>,
    _listener: Disposer,
}
impl Broadcast {
    pub fn listen(ctx: &Ctx, session_id: &str) -> Self {
        let records: Arc<std::sync::Mutex<Vec<Record>>> = Arc::default();
        let changed = Arc::new(tokio::sync::Notify::new());
        let listener =
            ctx.listen::<SessionBroadcast>()
                .name(session_channel(session_id))
                .on({
                    let records = Arc::clone(&records);
                    let changed = changed.clone();
                    move |event, _| {
                        let record = match &event.record {
                            SessionBroadcastEvent::Event { branch_id, event } => {
                                Some(Some((branch_id.clone(), event.clone())))
                            }
                            SessionBroadcastEvent::Closed => Some(None),
                            SessionBroadcastEvent::Notice(_)
                            | SessionBroadcastEvent::Fence { .. } => None,
                        };
                        if let Some(record) = record {
                            records.lock().unwrap().push(record);
                            changed.notify_waiters();
                        }
                        async { Ok(None) }
                    }
                })
                .unwrap();
        Self {
            records,
            changed,
            _listener: listener,
        }
    }
    /// Every record up to and including the end record.
    pub async fn ended(&self) -> Vec<Record> {
        loop {
            let notified = self.changed.notified();
            {
                let records = self.records.lock().unwrap();
                if records.last().is_some_and(Option::is_none) {
                    return records.clone();
                }
            }
            notified.await;
        }
    }
}

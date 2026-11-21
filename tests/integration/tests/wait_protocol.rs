use crate::task_store;
use dim_agent_capabilities::{
    prompt_guide::GuideEntries,
    timeline_tree::{TimelineTreePlugin, TreeTimelinePlugin},
    wait::{Wait, WaitBranchPlugin, WaitResult},
    workspace_tools::background::WAKE_TASK_TAG,
};
use dim_agent_runtime::{testing::MinimalHost, *};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Start,
    End,
    TaskA,
    TaskB,
    Steer,
    Plain,
    Consume,
    Delete,
    Reload,
    TakeA,
    TakeB,
}

struct IdleLoop(tokio::sync::watch::Sender<LoopSnapshot>);
impl LoopState for IdleLoop {
    fn state(&self) -> LoopSnapshot {
        LoopSnapshot::default()
    }
    fn watch(&self) -> tokio::sync::watch::Receiver<LoopSnapshot> {
        self.0.subscribe()
    }
}

type ProbedBranch = Arc<Mutex<Option<(Arc<Timeline>, Ctx)>>>;

struct Probe {
    idle: Key<dyn LoopState>,
    timeline: Key<Timeline>,
    seen: ProbedBranch,
}

impl DynPlugin for Probe {
    fn name(&self) -> &str {
        "test.wait-protocol"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.provide_at(
                self.idle.clone(),
                Arc::new(IdleLoop(
                    tokio::sync::watch::channel(LoopSnapshot::default()).0,
                )) as Arc<dyn LoopState>,
            )?;
            *self.seen.lock().unwrap() = Some((ctx.require_at(&self.timeline)?, ctx.clone()));
            Ok(Effect::Done)
        })
    }
}

/// The input id a trace action appends, so a later `Consume` or `Delete`
/// names it.
fn input_id(position: usize) -> String {
    format!("input-{position}")
}

/// The task a notice carries; a steer carries none.
fn task(action: Action) -> Option<&'static str> {
    match action {
        Action::TaskA => Some("a"),
        Action::TaskB => Some("b"),
        _ => None,
    }
}

/// What the recorded actions leave on the branch: the arrivals still released
/// and unread, in release order, and those the model read in this turn.
#[derive(Default)]
struct Record {
    live: Vec<usize>,
    read: Vec<usize>,
}

/// Replays the record the actions leave. `Start` opens the turn and `End`
/// closes and clears it; `Reload` clears the arrivals but keeps the turn. An
/// input released during an open turn arrives, and `Consume` or `Delete`
/// retires the latest arrival still on record, as the test writer's own action
/// does; only `Consume` records its task as read.
fn replay(history: &[Action]) -> Record {
    let mut record = Record::default();
    let mut open = false;
    for (position, action) in history.iter().enumerate() {
        match action {
            Action::Start => open = true,
            Action::End => {
                open = false;
                record = Record::default();
            }
            Action::Reload => record = Record::default(),
            Action::TaskA | Action::TaskB | Action::Steer => {
                if open {
                    record.live.push(position);
                }
            }
            Action::Consume | Action::Delete => {
                if let Some(position) = record.live.pop()
                    && *action == Action::Consume
                {
                    record.read.push(position);
                }
            }
            Action::Plain | Action::TakeA | Action::TakeB => {}
        }
    }
    record
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    Steer,
    Notification,
    AlreadyNotified,
    Timeout,
}

/// How many calls returned each reason, so the run shows every trace family
/// reached the branch it exercises.
#[derive(Default)]
struct Cover {
    steer: usize,
    notification: usize,
    already_notified: usize,
    timeout: usize,
}

impl Cover {
    fn add(&mut self, reason: Reason) {
        let count = match reason {
            Reason::Steer => &mut self.steer,
            Reason::Notification => &mut self.notification,
            Reason::AlreadyNotified => &mut self.already_notified,
            Reason::Timeout => &mut self.timeout,
        };
        *count += 1;
    }
}

#[derive(Clone, Copy, Debug)]
struct Expected {
    reason: Reason,
    /// The history position the call spends, if it takes an arrival.
    position: Option<usize>,
    /// The task the result names, if it names one.
    task: Option<&'static str>,
}

/// The result the contract requires after `history`, given the positions
/// earlier calls returned.
fn expected(history: &[Action], taken: &[usize], task_id: Option<&str>) -> Expected {
    let record = replay(history);
    let available: Vec<usize> = record
        .live
        .iter()
        .copied()
        .filter(|position| !taken.contains(position))
        .collect();
    if let Some(position) = available
        .iter()
        .copied()
        .find(|position| history[*position] == Action::Steer)
    {
        return Expected {
            reason: Reason::Steer,
            position: Some(position),
            task: None,
        };
    }
    if let Some(position) = available.iter().copied().find(|position| {
        task(history[*position]).is_some_and(|id| task_id.is_none_or(|task| task == id))
    }) {
        return Expected {
            reason: Reason::Notification,
            position: Some(position),
            task: task(history[position]),
        };
    }
    if let Some(position) = record
        .read
        .iter()
        .copied()
        .find(|position| task_id.is_some_and(|id| task(history[*position]) == Some(id)))
    {
        return Expected {
            reason: Reason::AlreadyNotified,
            position: None,
            task: task(history[position]),
        };
    }
    Expected {
        reason: Reason::Timeout,
        position: None,
        task: None,
    }
}

/// The single task id a result names.
fn one(task_ids: &[String], history: &[Action]) -> Option<String> {
    assert_eq!(
        task_ids.len(),
        1,
        "history={history:?}, task ids={task_ids:?}"
    );
    task_ids.first().cloned()
}

async fn compare(
    session: &Session,
    history: &[Action],
    taken: &mut Vec<usize>,
    task_id: Option<&str>,
    cover: &mut Cover,
) {
    let expected = expected(history, taken, task_id);
    let result = session
        .service::<Wait>("main")
        .unwrap()
        .wait(
            task_id.map(str::to_owned),
            0,
            Arc::new(|| Box::pin(std::future::pending())),
        )
        .await
        .unwrap();
    let (reason, named) = match &result {
        WaitResult::Notification { task_ids, .. } => (Reason::Notification, one(task_ids, history)),
        WaitResult::UserSteer { .. } => (Reason::Steer, None),
        WaitResult::AlreadyNotified { task_ids, .. } => {
            (Reason::AlreadyNotified, one(task_ids, history))
        }
        WaitResult::Timeout { .. } => (Reason::Timeout, None),
        other => panic!("unexpected result for {history:?}: {other:?}"),
    };
    assert_eq!(
        reason, expected.reason,
        "history={history:?}, taken={taken:?}, task={task_id:?}, result={result:?}"
    );
    assert_eq!(
        named.as_deref(),
        expected.task,
        "history={history:?}, taken={taken:?}, task={task_id:?}, result={result:?}"
    );
    if let Some(position) = expected.position {
        taken.push(position);
    }
    cover.add(reason);
}

#[tokio::test]
async fn wait_timeline_traces_match_the_notification_contract() {
    use Action::*;
    let seen = Arc::new(Mutex::new(None));
    let capture = seen.clone();
    let store = Arc::new(task_store::TaskStore::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::register_names(&mut builder);
    builder
        .session_value(Arc::new(GuideEntries::default()))
        .require_feature(
            Feature::new("TimelineTree")
                .session("TimelineTree", |names| {
                    Ok(Box::new(TimelineTreePlugin::new(names)?))
                })
                .branch("TreeTimeline", |names| {
                    Ok(Box::new(TreeTimelinePlugin::new(names)?))
                }),
        )
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()))
        .require_feature(Feature::new("Wait").branch("WaitBranch", |names| {
            Ok(Box::new(WaitBranchPlugin::new(names)?))
        }))
        .require_feature(Feature::new("test.wait-protocol").branch(
            "test.wait-protocol",
            move |names| {
                Ok(Box::new(Probe {
                    idle: names.key()?,
                    timeline: names.key()?,
                    seen: capture.clone(),
                }))
            },
        ));
    let host = MinimalHost::start(&builder).await.unwrap();
    let alphabet = [
        Start, End, TaskA, TaskB, Steer, Plain, Consume, Delete, Reload, TakeA, TakeB,
    ];
    let mut traces = Vec::new();
    for a in alphabet {
        for b in alphabet {
            for c in alphabet {
                traces.push(vec![a, b, c]);
            }
        }
    }
    traces.extend([
        vec![Start, TaskB, TaskA, Steer, TakeA, TakeA],
        vec![Start, TaskA, Reload, TaskB, TakeA],
        vec![Start, TaskA, End, Start, TaskB],
        vec![Start, TaskA, TakeA, TaskA, TakeA],
        vec![Start, TaskA, Steer, Consume, Consume, TakeA],
        vec![Start, TaskB, TaskA, Consume, TakeA, TakeA],
        vec![Start, TaskA, Consume, TakeA, TakeA],
        vec![Start, TaskB, Consume, TakeB, TakeB],
        vec![Start, TaskB, Delete, TakeB, TakeB],
        vec![Start, TaskB, Consume, End, TakeB],
        vec![Start, TaskB, Consume, Reload, TakeB],
        vec![Start, TaskA, TakeA, Consume, TakeA],
        vec![Start, TaskA, TaskB, Consume, TakeB],
        vec![Start, TaskB, Consume, TaskB, TakeB, TakeB],
    ]);
    let mut cover = Cover::default();
    for trace in &traces {
        let session = host.create_session(SessionConfig::default()).await.unwrap();
        let (timeline, ctx) = seen.lock().unwrap().clone().unwrap();
        let mut history = Vec::new();
        let mut taken = Vec::new();
        for (position, action) in trace.iter().enumerate() {
            match action {
                Reload => {
                    let id = session
                        .branch_diagnostics("main")
                        .unwrap()
                        .plugins
                        .iter()
                        .find(|p| p.name == "WaitBranch")
                        .unwrap()
                        .id;
                    ctx.view(id).unwrap().restart().await.unwrap();
                }
                TakeA => compare(&session, &history, &mut taken, Some("a"), &mut cover).await,
                TakeB => compare(&session, &history, &mut taken, Some("b"), &mut cover).await,
                Consume | Delete => {
                    let latest = replay(&history).live.last().copied();
                    if let Some(latest) = latest {
                        let kind = if *action == Consume {
                            EventKind::InputConsumed(InputConsumedData {
                                input_id: input_id(latest),
                            })
                        } else {
                            EventKind::InputDeleted(InputDeletedData {
                                input_id: input_id(latest),
                            })
                        };
                        timeline.append(kind.into()).unwrap();
                        session.idle("main").await.unwrap();
                    }
                }
                _ => {
                    let draft = match action {
                        Start => EventKind::TurnStart(TurnStartData {
                            turn_id: "turn".into(),
                        })
                        .into(),
                        End => EventKind::TurnEnd(TurnEndData {
                            turn_id: "turn".into(),
                            outcome: TurnOutcome::Completed {
                                reason: "done".into(),
                            },
                        })
                        .into(),
                        TaskA | TaskB | Steer | Plain => {
                            let mut draft =
                                EventDraft::from(EventKind::InputMessage(InputMessageData::text(
                                    "notice",
                                    Some(if *action == Plain {
                                        InputMode::Queue
                                    } else {
                                        InputMode::Steer
                                    }),
                                )))
                                .with_id(input_id(position));
                            if matches!(action, TaskA | TaskB) {
                                let task = if *action == TaskA { "a" } else { "b" };
                                draft.tags = Some(vec![format!("{WAKE_TASK_TAG}{task}")]);
                            }
                            draft
                        }
                        _ => unreachable!(),
                    };
                    timeline.append(draft).unwrap();
                    session.idle("main").await.unwrap();
                    if matches!(action, TaskA | TaskB | Steer | Plain) {
                        timeline
                            .append(
                                EventKind::InputApplied(InputAppliedData {
                                    input_id: input_id(position),
                                    mode: None,
                                })
                                .into(),
                            )
                            .unwrap();
                        session.idle("main").await.unwrap();
                    }
                }
            }
            history.push(*action);
        }
        compare(&session, &history, &mut taken, Some("a"), &mut cover).await;
        for _ in 0..=history.len() {
            compare(&session, &history, &mut taken, None, &mut cover).await;
        }
        session.close().await.unwrap();
    }
    for initially_open in [false, true] {
        let session = host.create_session(SessionConfig::default()).await.unwrap();
        let (timeline, ctx) = seen.lock().unwrap().clone().unwrap();
        if initially_open {
            timeline
                .append(
                    EventKind::TurnStart(TurnStartData {
                        turn_id: "turn".into(),
                    })
                    .into(),
                )
                .unwrap();
            session.idle("main").await.unwrap();
        }
        let id = session
            .branch_diagnostics("main")
            .unwrap()
            .plugins
            .iter()
            .find(|p| p.name == "WaitBranch")
            .unwrap()
            .id;
        let release = store.hold_next_query();
        let mut restarting = ctx.view(id).unwrap().restart();
        tokio::select! {
            result = &mut restarting => panic!("restart bypassed held query: {result:?}"),
            _ = store.entered.notified() => {},
        }
        let boundary = if initially_open {
            EventKind::TurnEnd(TurnEndData {
                turn_id: "turn".into(),
                outcome: TurnOutcome::Completed {
                    reason: "done".into(),
                },
            })
        } else {
            EventKind::TurnStart(TurnStartData {
                turn_id: "turn".into(),
            })
        };
        timeline.append(boundary.into()).unwrap();
        let steer = EventDraft::from(EventKind::InputMessage(InputMessageData::text(
            "during recovery",
            Some(InputMode::Steer),
        )))
        .with_id(input_id(0));
        timeline.append(steer).unwrap();
        timeline
            .append(
                EventKind::InputApplied(InputAppliedData {
                    input_id: input_id(0),
                    mode: None,
                })
                .into(),
            )
            .unwrap();
        session.idle("main").await.unwrap();
        release.send(Ok(())).unwrap();
        restarting.await.unwrap();
        let history = if initially_open {
            vec![Start, Reload, End, Steer]
        } else {
            vec![Reload, Start, Steer]
        };
        let mut taken = Vec::new();
        let mut recovery_cover = Cover::default();
        compare(&session, &history, &mut taken, None, &mut recovery_cover).await;
        compare(&session, &history, &mut taken, None, &mut recovery_cover).await;
        let outcomes = (recovery_cover.steer, recovery_cover.timeout);
        assert_eq!(
            outcomes,
            if initially_open { (0, 2) } else { (1, 1) },
            "the post-cut boundary decides whether the released steer can end a wait"
        );
        session.close().await.unwrap();
    }
    host.close().await.unwrap();
    println!(
        "checked {} real Timeline/Dispatcher/Wait traces: {} steer, {} \
         notification, {} already notified, {} timeout",
        traces.len(),
        cover.steer,
        cover.notification,
        cover.already_notified,
        cover.timeout
    );
    assert_eq!(
        [
            cover.steer > 0,
            cover.notification > 0,
            cover.already_notified > 0,
            cover.timeout > 0,
        ],
        [true; 4],
        "every trace family must reach its outcome"
    );
}

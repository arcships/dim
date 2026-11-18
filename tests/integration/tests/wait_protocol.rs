#[path = "local_support/task_store.rs"]
mod task_store;
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
    Reload,
    TakeA,
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

fn expected(history: &[Action], taken: &[usize], task: Option<&str>) -> Option<usize> {
    let candidates: Vec<_> = history
        .iter()
        .enumerate()
        .filter(|(i, action)| {
            matches!(action, Action::TaskA | Action::TaskB | Action::Steer)
                && !taken.contains(i)
                && history[..*i]
                    .iter()
                    .rev()
                    .find(|a| matches!(a, Action::Start | Action::End))
                    == Some(&Action::Start)
                && !history[i + 1..]
                    .iter()
                    .any(|a| matches!(a, Action::End | Action::Reload))
        })
        .collect();
    candidates
        .iter()
        .find(|(_, a)| **a == Action::Steer)
        .or_else(|| {
            candidates.iter().find(|(_, a)| match a {
                Action::TaskA => task.is_none_or(|id| id == "a"),
                Action::TaskB => task.is_none_or(|id| id == "b"),
                _ => false,
            })
        })
        .map(|(i, _)| *i)
}

async fn compare(
    session: &Session,
    history: &[Action],
    taken: &mut Vec<usize>,
    task: Option<&str>,
) {
    let expected = expected(history, taken, task);
    let result = session
        .service::<Wait>("main")
        .unwrap()
        .wait(
            task.map(str::to_owned),
            0,
            Arc::new(|| Box::pin(std::future::pending())),
        )
        .await
        .unwrap();
    let actual = match result {
        WaitResult::Notification { task_ids, .. } => {
            assert_eq!(task_ids.len(), 1);
            Some(if task_ids[0] == "a" {
                Action::TaskA
            } else {
                assert_eq!(task_ids[0], "b");
                Action::TaskB
            })
        }
        WaitResult::UserSteer { .. } => Some(Action::Steer),
        WaitResult::Timeout { .. } => None,
        other => panic!("unexpected result for {history:?}: {other:?}"),
    };
    assert_eq!(
        actual,
        expected.map(|i| history[i]),
        "history={history:?}, taken={taken:?}, task={task:?}"
    );
    if let Some(i) = expected {
        taken.push(i);
    }
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
    let alphabet = [Start, End, TaskA, TaskB, Steer, Plain, Reload, TakeA];
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
    ]);
    for trace in &traces {
        let session = host.create_session(SessionConfig::default()).await.unwrap();
        let (timeline, ctx) = seen.lock().unwrap().clone().unwrap();
        let mut history = Vec::new();
        let mut taken = Vec::new();
        for action in trace {
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
                TakeA => compare(&session, &history, &mut taken, Some("a")).await,
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
                                )));
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
                }
            }
            history.push(*action);
        }
        compare(&session, &history, &mut taken, Some("a")).await;
        for _ in 0..=history.len() {
            compare(&session, &history, &mut taken, None).await;
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
        timeline
            .append(
                EventKind::InputMessage(InputMessageData::text(
                    "during recovery",
                    Some(InputMode::Steer),
                ))
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
        compare(&session, &history, &mut taken, None).await;
        compare(&session, &history, &mut taken, None).await;
        session.close().await.unwrap();
    }
    host.close().await.unwrap();
    println!(
        "checked {} real Timeline/Dispatcher/Wait traces",
        traces.len()
    );
}

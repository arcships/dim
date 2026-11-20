//! ProgressReminder: asks the model for reply text after a long silent
//! stretch of tool calls. The reminder is an unkeyed Append input, so it rides
//! with the line's next model request and never stays in effect.
use crate::{
    context_block,
    prompt_guide::{GuideEntries, GuideEntry, GuideScope},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{
    BranchLifecycle, Dispatcher, Names, QueryFilter, QueryOperand, QueryOutput, QueryPage,
    QueryPick, QuerySelect, QuerySpec, QueryValue, Result, SessionConfig, SessionLifecycle,
    Timeline, TimelineIdentity, TimelineQuery, TimelineQueryScope, core::unix_millis,
};
use dim_agent_types::{AgentEvent, EventKind, InputMessageData, InputMode, TimelinePosition};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    num::NonZeroU64,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::Notify,
    time::{Instant, sleep_until},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// The marker the reminder carries.
pub const REMINDER: &str = "dim-progress-reminder";

/// The guide entry for [`REMINDER`].
const ENTRY: &str = r#"<dim-progress-reminder seconds="61">: a note from the runtime that the user has seen no reply text from you for about the stated number of seconds while you worked through tool calls. Give the user a one- or two-sentence update in your reply text, then continue."#;

/// How long the line may work without writing text before the runtime
/// reminds it ([`ProgressReminderConfig::after`]).
const DEFAULT_AFTER: Duration = Duration::from_secs(60);

/// The facts the reminder is decided from.
const KEYS: [&str; 4] = ["turn/start", "turn/end", "step/text", "step/tool-use"];

/// ProgressReminder's factory configuration.
#[derive(Clone, Copy, Debug)]
pub struct ProgressReminderConfig {
    /// A silent stretch that reaches this length earns one reminder. The
    /// product keeps the default; tests set it.
    pub after: Duration,
}
impl Default for ProgressReminderConfig {
    fn default() -> Self {
        Self {
            after: DEFAULT_AFTER,
        }
    }
}

pub struct ProgressReminderPlugin {
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    identity: Key<TimelineIdentity>,
    tree: Key<TreeCommands>,
    config: Key<SessionConfig>,
    lifecycle: Key<BranchLifecycle>,
    session: Key<SessionLifecycle>,
    guide: Key<GuideEntries>,
    reminder: ProgressReminderConfig,
}
impl ProgressReminderPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Self::with_config(names, ProgressReminderConfig::default())
    }
    /// The plugin with a candidate's threshold at its factory configuration.
    pub fn with_config(names: &Names<'_>, reminder: ProgressReminderConfig) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
            dispatcher: names.key()?,
            identity: names.key()?,
            tree: names.key()?,
            config: names.key()?,
            lifecycle: names.key()?,
            session: names.key()?,
            guide: names.key()?,
            reminder,
        })
    }
}
impl DynPlugin for ProgressReminderPlugin {
    fn name(&self) -> &str {
        "ProgressReminder"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.identity.erase(),
            self.tree.erase(),
            self.config.erase(),
            self.lifecycle.erase(),
            self.session.erase(),
            self.guide.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let config = ctx.require_at(&self.config)?;
            if !config.get().interactive {
                return Ok(Effect::Done);
            }
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let tree = ctx.require_at(&self.tree)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let session = ctx.require_at(&self.session)?;
            let guide = ctx.require_at(&self.guide)?;
            guide.register(
                ctx,
                GuideScope::Branch(identity.branch_id.clone()),
                GuideEntry {
                    markers: vec![REMINDER.into()],
                    order: 60,
                    text: ENTRY.into(),
                },
            )?;
            let owner = Arc::new(Reminder {
                own: identity.branch_id.clone(),
                tree,
                timeline: timeline.clone(),
                after: self.reminder.after,
                state: Mutex::new(Stretch::default()),
                changed: Notify::new(),
                stop: CancellationToken::new(),
            });
            let rebuilding = owner.clone();
            let consuming = owner.clone();
            dispatcher
                .consume(
                    ctx,
                    "ProgressReminder".into(),
                    KEYS.map(String::from).to_vec(),
                    Arc::new(move || {
                        let consumer = rebuilding.clone();
                        Box::pin(async move { consumer.rebuild().await })
                    }),
                    Arc::new(move |event| {
                        let consumer = consuming.clone();
                        Box::pin(async move {
                            consumer.observe(&event);
                            Ok(())
                        })
                    }),
                )
                .await?;
            let tasks = TaskTracker::new();
            let writer = owner.clone();
            tasks.spawn(async move {
                tokio::select! {
                    _ = writer.stop.cancelled() => return,
                    active = lifecycle.activated() => if !active { return },
                }
                if !session.activated().await {
                    return;
                }
                writer.run().await;
            });
            let stop = owner.stop.clone();
            ctx.effect(move || async move {
                stop.cancel();
                tasks.close();
                tasks.wait().await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

/// One line's current silent stretch.
#[derive(Default)]
struct Stretch {
    /// The open turn, if any.
    turn: Option<String>,
    /// When the stretch started: the turn's start, the latest text or the
    /// latest reminder.
    since: Option<Instant>,
    /// The model made a tool call since the stretch started.
    tool: bool,
}
impl Stretch {
    /// The deadline that earns the next reminder.
    fn deadline(&self, after: Duration) -> Option<Instant> {
        if !self.tool || self.turn.is_none() {
            return None;
        }
        self.since.map(|since| since + after)
    }
}

struct Reminder {
    own: String,
    tree: Arc<TreeCommands>,
    timeline: Arc<Timeline>,
    after: Duration,
    state: Mutex<Stretch>,
    changed: Notify,
    stop: CancellationToken,
}
impl Reminder {
    /// Rebuilds the current stretch from the facts the read holds, so a reload
    /// continues the same stretch instead of reminding twice (plugin contract
    /// §5.2).
    async fn rebuild(&self) -> Result<u64> {
        let through = self.timeline.committed();
        *self.state.lock().unwrap() = rebuilt(&self.timeline, &self.own, through).await?;
        Ok(through)
    }
    /// Folds one live fact.
    fn observe(&self, event: &AgentEvent) {
        if event.parent_call_id.is_some() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        match &event.kind {
            EventKind::TurnStart(data) => {
                state.turn = Some(data.turn_id.clone());
                state.since = Some(at(event.create_at));
                state.tool = false;
            }
            EventKind::TurnEnd(data) => {
                if state.turn.as_deref() != Some(data.turn_id.as_str()) {
                    return;
                }
                state.turn = None;
                state.since = None;
                state.tool = false;
            }
            EventKind::Text(data) if !data.delta.is_empty() => {
                if state.turn.is_none() {
                    return;
                }
                state.since = Some(at(event.create_at));
                state.tool = false;
            }
            EventKind::ToolUse(_) => {
                if state.turn.is_none() {
                    return;
                }
                state.tool = true;
            }
            _ => return,
        }
        drop(state);
        self.changed.notify_one();
    }
    /// Waits for a deadline, rechecks the stretch, and writes the reminder.
    async fn run(&self) {
        loop {
            let notified = self.changed.notified();
            let deadline = self.state.lock().unwrap().deadline(self.after);
            let due = async {
                match deadline {
                    Some(deadline) => sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = self.stop.cancelled() => return,
                _ = notified => continue,
                _ = due => {}
            }
            if self.stop.is_cancelled() {
                return;
            }
            if self.remind().await.is_err() {
                return;
            }
        }
    }
    /// The recheck before the write: the same turn, still no text, a tool
    /// call, and the stretch is due.
    async fn remind(&self) -> Result<()> {
        let (turn, since) = {
            let state = self.state.lock().unwrap();
            if !state.tool {
                return Ok(());
            }
            let (Some(turn), Some(since)) = (state.turn.clone(), state.since) else {
                return Ok(());
            };
            (turn, since)
        };
        let now = Instant::now();
        if now < since + self.after {
            return Ok(());
        }
        let seconds = now
            .saturating_duration_since(since)
            .as_secs()
            .max(self.after.as_secs());
        let body = format!(
            "The user has seen no text from you for about {seconds} seconds while you worked. Tell them in one or two short sentences what you are doing, then continue."
        );
        let block = context_block::raw(
            context_block::element(REMINDER, &[("seconds", &seconds.to_string())], &body),
            None,
        );
        let data = InputMessageData {
            blocks: vec![block],
            mode: Some(InputMode::Append),
            ..InputMessageData::default()
        };
        self.tree.input(self.own.clone(), data).await?;
        let mut state = self.state.lock().unwrap();
        if state.turn.as_deref() == Some(turn.as_str()) {
            state.since = Some(Instant::now());
            state.tool = false;
        }
        drop(state);
        self.changed.notify_one();
        Ok(())
    }
}

/// The stretch the committed facts state, read at a reload.
async fn rebuilt(timeline: &Timeline, own: &str, through: u64) -> Result<Stretch> {
    let Some((turn, start, start_at)) = latest_turn(timeline, own, through).await? else {
        return Ok(Stretch::default());
    };
    let (anchor, anchor_at) = latest_text(timeline, own, start, through)
        .await?
        .unwrap_or((start, start_at));
    let (anchor, anchor_at) = latest_reminder(timeline, own, anchor, through)
        .await?
        .unwrap_or((anchor, anchor_at));
    let tool = exists(timeline, own, anchor, through, "step/tool-use").await?;
    Ok(Stretch {
        turn: Some(turn),
        since: Some(at(anchor_at)),
        tool,
    })
}

/// The open turn, its start position and its time, if the latest turn
/// boundary is a start.
async fn latest_turn(
    timeline: &Timeline,
    own: &str,
    through: u64,
) -> Result<Option<(String, u64, u64)>> {
    let Some(row) = last(
        timeline,
        own,
        None,
        through,
        &["turn/start", "turn/end"],
        &["kind", "data.turn_id", "position.sequence", "create_at"],
    )
    .await?
    else {
        return Ok(None);
    };
    let [kind, turn, sequence, at] = row.as_slice() else {
        unreachable!("turn boundary fields")
    };
    let (Some("turn/start"), Some(turn), Some(sequence), Some(at)) =
        (string(kind), string(turn), number(sequence), number(at))
    else {
        return Ok(None);
    };
    Ok(Some((turn.to_owned(), sequence, at)))
}

/// The latest text position and time of the open turn.
async fn latest_text(
    timeline: &Timeline,
    own: &str,
    start: u64,
    through: u64,
) -> Result<Option<(u64, u64)>> {
    let Some(row) = last(
        timeline,
        own,
        Some(start),
        through,
        &["step/text"],
        &["position.sequence", "create_at"],
    )
    .await?
    else {
        return Ok(None);
    };
    let [sequence, at] = row.as_slice() else {
        unreachable!("text fields")
    };
    Ok(number(sequence).zip(number(at)))
}

/// The position and time of the latest reminder after `after`.
async fn latest_reminder(
    timeline: &Timeline,
    own: &str,
    after: u64,
    through: u64,
) -> Result<Option<(u64, u64)>> {
    if after >= through {
        return Ok(None);
    }
    let reply = timeline
        .query(TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: Some(TimelinePosition {
                branch_id: own.into(),
                sequence: after.saturating_add(1),
            }),
            through: Some(through),
            include_publishing: false,
            query: QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![
                        QueryFilter::In {
                            value: field("kind"),
                            values: vec!["input/message".into()],
                        },
                        QueryFilter::Any {
                            field: "data.blocks".into(),
                            condition: Box::new(QueryFilter::Prefix {
                                value: QueryOperand::Element {
                                    path: "text".into(),
                                },
                                prefix: format!("<{REMINDER}"),
                            }),
                        },
                    ],
                }),
                select: QuerySelect::Fields {
                    fields: vec!["position.sequence".into(), "create_at".into()],
                },
                group_by: vec![],
                pick: Some(QueryPick::Last),
                order_by: vec![],
            },
            page: Some(QueryPage {
                limit: NonZeroU64::MIN,
                after: None,
            }),
        })
        .await?;
    let QueryOutput::Rows { rows } = reply.result else {
        unreachable!("reminder query")
    };
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let [sequence, at] = row.values.as_slice() else {
        unreachable!("reminder fields")
    };
    Ok(number(sequence).zip(number(at)))
}

/// Whether a fact of `kind` is committed after `after`.
async fn exists(
    timeline: &Timeline,
    own: &str,
    after: u64,
    through: u64,
    kind: &str,
) -> Result<bool> {
    if after >= through {
        return Ok(false);
    }
    let reply = timeline
        .query(TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: Some(TimelinePosition {
                branch_id: own.into(),
                sequence: after.saturating_add(1),
            }),
            through: Some(through),
            include_publishing: false,
            query: QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![
                        QueryFilter::In {
                            value: field("kind"),
                            values: vec![kind.into()],
                        },
                        QueryFilter::Not {
                            condition: Box::new(QueryFilter::Present {
                                value: field("parent_call_id"),
                            }),
                        },
                    ],
                }),
                select: QuerySelect::Exists,
                group_by: vec![],
                pick: None,
                order_by: vec![],
            },
            page: None,
        })
        .await?;
    let QueryOutput::Exists { exists } = reply.result else {
        unreachable!("existence query")
    };
    Ok(exists)
}

/// The last row of `kinds` after `from`.
async fn last(
    timeline: &Timeline,
    own: &str,
    from: Option<u64>,
    through: u64,
    kinds: &[&str],
    fields: &[&str],
) -> Result<Option<Vec<QueryValue>>> {
    let reply = timeline
        .query(TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: from.map(|sequence| TimelinePosition {
                branch_id: own.into(),
                sequence,
            }),
            through: Some(through),
            include_publishing: false,
            query: QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![
                        QueryFilter::In {
                            value: field("kind"),
                            values: kinds.iter().map(|kind| (*kind).into()).collect(),
                        },
                        QueryFilter::Not {
                            condition: Box::new(QueryFilter::Present {
                                value: field("parent_call_id"),
                            }),
                        },
                    ],
                }),
                select: QuerySelect::Fields {
                    fields: fields.iter().map(|field| (*field).into()).collect(),
                },
                group_by: vec![],
                pick: Some(QueryPick::Last),
                order_by: vec![],
            },
            page: Some(QueryPage {
                limit: NonZeroU64::MIN,
                after: None,
            }),
        })
        .await?;
    let QueryOutput::Rows { rows } = reply.result else {
        unreachable!("row query")
    };
    Ok(rows.into_iter().next().map(|row| row.values))
}

fn field(path: &str) -> QueryOperand {
    QueryOperand::Field { path: path.into() }
}

fn string(value: &QueryValue) -> Option<&str> {
    match value {
        QueryValue::Value {
            value: serde_json::Value::String(text),
        } => Some(text),
        _ => None,
    }
}

fn number(value: &QueryValue) -> Option<u64> {
    match value {
        QueryValue::Value {
            value: serde_json::Value::Number(number),
        } => number.as_u64(),
        _ => None,
    }
}

/// Maps an event's wall-clock time onto the runtime clock, so a rebuilt
/// stretch keeps its age and the writer's timer is testable through tokio's
/// paused clock.
fn at(create_at: u64) -> Instant {
    let age = unix_millis().saturating_sub(create_at);
    Instant::now() - Duration::from_millis(age)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dim_agent_runtime::MemoryTimelineStore;
    use dim_agent_runtime::{TimelineKey, TimelineStore};
    use dim_agent_types::{ToolCall, ToolUseData, TurnStartData};
    use std::sync::Arc;

    async fn timeline() -> Timeline {
        let store = Arc::new(MemoryTimelineStore::default());
        store
            .create_session("s", &SessionConfig::default())
            .await
            .unwrap();
        Timeline::new(
            Ctx::root().unwrap(),
            TimelineKey {
                session_id: "s".into(),
                branch_id: "main".into(),
            },
            vec![],
            store,
        )
    }

    #[tokio::test]
    async fn a_rebuilt_stretch_needs_a_tool_call_after_the_reminder() {
        let timeline = timeline().await;
        let now = unix_millis();
        timeline
            .append_with_create_at(
                EventKind::TurnStart(TurnStartData {
                    turn_id: "t1".into(),
                })
                .into(),
                now - 61_000,
            )
            .unwrap();
        timeline
            .append_with_create_at(
                EventKind::ToolUse(ToolUseData {
                    turn_id: "t1".into(),
                    request_id: "r1".into(),
                    call: ToolCall {
                        id: "c1".into(),
                        name: "gate".into(),
                        input: Default::default(),
                    },
                })
                .into(),
                now - 60_000,
            )
            .unwrap();
        let block = context_block::raw(
            context_block::element(REMINDER, &[("seconds", "60")], "reminder"),
            None,
        );
        timeline
            .append_with_create_at(
                EventKind::InputMessage(InputMessageData {
                    blocks: vec![block],
                    mode: Some(InputMode::Append),
                    ..InputMessageData::default()
                })
                .into(),
                now - 30_000,
            )
            .unwrap();
        let stretch = rebuilt(&timeline, "main", timeline.committed())
            .await
            .unwrap();
        assert_eq!(stretch.turn.as_deref(), Some("t1"));
        assert!(
            !stretch.tool,
            "the tool call before the reminder does not count"
        );
        let age = stretch.since.expect("a start").elapsed().as_secs();
        assert!(
            (28..=32).contains(&age),
            "the stretch starts at the reminder, not the turn: {age}s"
        );
    }
}

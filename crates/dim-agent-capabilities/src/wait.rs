//! The `wait` tool, see `docs/architecture/rust-agent/plugins/Wait.md`; definition, rules
//! and content copy TS `packages/agent/src/tool/wait-plugin.ts`.
//!
//! `WaitPlugin` (Session) contributes the tool; `WaitBranchPlugin` provides
//! `B:Wait`, which records the background completion notifications and
//! steers dispatched on its branch between `turn/start` and `turn/end` (the
//! input that opened the turn comes before `turn/start`, so it never ends a
//! wait, as TS only commits inputs of a running turn).
use crate::{
    host::{CancelledFn, HostError, HostFuture, ToolExecutionContext, call_until_cancelled},
    timeline_tree::TreeCommands,
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry, definition},
    workspace_tools::background::{WAKE_TASK_TAG, tagged},
};
use dim_agent_runtime::{
    AgentBuilder, AgentError, CAPABILITY_RELOADING, Dispatcher, Names, QueryFilter, QueryOperand,
    QueryOutput, QueryPage, QueryPick, QuerySelect, QuerySpec, QueryValue, Timeline, TimelineQuery,
    TimelineQueryScope,
};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    num::NonZeroU64,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const WAIT_TOOL: &str = "wait";
const DEFAULT_TIMEOUT_MS: u64 = 300_000;

/// The Wait plugin: the Session tool and the branch part.
pub fn feature() -> dim_agent_runtime::Feature {
    dim_agent_runtime::Feature::new("Wait")
        .session("Wait", |names| Ok(Box::new(WaitPlugin::new(names)?)))
        .branch("WaitBranch", |names| {
            Ok(Box::new(WaitBranchPlugin::new(names)?))
        })
}

/// Installs Wait in the profile.
pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(feature());
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Arrival {
    BackgroundTask(String),
    Steer,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(
    tag = "reason",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WaitResult {
    Notification {
        task_ids: Vec<String>,
        waited_ms: u64,
    },
    UserSteer {
        waited_ms: u64,
    },
    Timeout {
        waited_ms: u64,
    },
    Interrupted {
        waited_ms: u64,
    },
}

/// Another `wait` is running on this branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("wait is already running in this session.")]
pub struct AlreadyWaiting;

#[derive(Default)]
struct State {
    pending: VecDeque<Arrival>,
    waiting: bool,
    /// Between a dispatched `turn/start` and its `turn/end`.
    open: bool,
}

pub struct Wait {
    state: Mutex<State>,
    arrived: Notify,
}

#[rutis::service]
impl Wait {
    /// Waits for the next matching arrival, the timeout or the caller's
    /// cancellation, as the `wait` tool's call does.
    ///
    /// The caller cancels the call with `cancelled`; the wait then ends as
    /// [`WaitResult::Interrupted`] and that answer stands. A caller that
    /// goes away without cancelling (its row unloads or the process ends)
    /// drops the call: no answer is produced and the branch's single wait
    /// slot is released, so the next `wait` on this branch starts.
    pub async fn wait(
        &self,
        task_id: Option<String>,
        timeout_ms: u64,
        cancelled: CancelledFn,
    ) -> Result<WaitResult, AlreadyWaiting> {
        let cancel = CancellationToken::new();
        let _dropped = cancel.clone().drop_guard();
        call_until_cancelled(
            cancelled,
            cancel.clone(),
            self.arrival(
                task_id.as_deref(),
                Duration::from_millis(timeout_ms),
                cancel,
            ),
        )
        .await
    }

    /// The last `turn/start` or `turn/end` of this branch decides whether a
    /// turn is open.
    async fn rebuild(&self, timeline: &Timeline) -> dim_agent_runtime::Result<u64> {
        let reply = timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: None,
                through: Some(timeline.committed()),
                include_publishing: true,
                query: QuerySpec {
                    filter: Some(QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: vec!["turn/start".into(), "turn/end".into()],
                    }),
                    select: QuerySelect::Fields {
                        fields: vec!["kind".into()],
                    },
                    group_by: Vec::new(),
                    pick: Some(QueryPick::Last),
                    order_by: Vec::new(),
                },
                page: Some(QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            })
            .await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("wait recovery selects the latest turn control kind")
        };
        let open = rows.first().is_some_and(|row| {
            matches!(row.values.first(),
            Some(QueryValue::Value { value: Value::String(kind) }) if kind == "turn/start")
        });
        self.state.lock().unwrap().open = open;
        Ok(reply.through)
    }
    fn consume(&self, event: &AgentEvent) {
        let mut state = self.state.lock().unwrap();
        match &event.kind {
            EventKind::TurnStart(_) => state.open = true,
            EventKind::TurnEnd(_) => {
                state.open = false;
                state.pending.clear();
            }
            EventKind::InputMessage(_) if !state.open => {}
            EventKind::InputMessage(data) => {
                if let Some(task_id) = tagged(event, WAKE_TASK_TAG) {
                    state
                        .pending
                        .push_back(Arrival::BackgroundTask(task_id.to_owned()));
                } else if data.effective_mode() == InputMode::Steer {
                    state.pending.push_back(Arrival::Steer);
                } else {
                    return;
                }
                drop(state);
                self.arrived.notify_waiters();
            }
            _ => {}
        }
    }

    /// TS `takeMatchingInput`: any steer first, then a matching task.
    fn take(&self, task_id: Option<&str>) -> Option<Arrival> {
        let mut state = self.state.lock().unwrap();
        let index = state
            .pending
            .iter()
            .position(|arrival| *arrival == Arrival::Steer)
            .or_else(|| {
                state.pending.iter().position(|arrival| match arrival {
                    Arrival::BackgroundTask(id) => task_id.is_none_or(|task_id| task_id == id),
                    Arrival::Steer => false,
                })
            })?;
        state.pending.remove(index)
    }

    /// Waits for the next matching arrival, the timeout or `cancel`.
    async fn arrival(
        &self,
        task_id: Option<&str>,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<WaitResult, AlreadyWaiting> {
        {
            let mut state = self.state.lock().unwrap();
            if state.waiting {
                return Err(AlreadyWaiting);
            }
            state.waiting = true;
        }
        struct Release<'a>(&'a Mutex<State>);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.lock().unwrap().waiting = false;
            }
        }
        let _release = Release(&self.state);
        let started = Instant::now();
        let waited = || started.elapsed().as_millis() as u64;
        if cancel.is_cancelled() {
            return Ok(WaitResult::Interrupted {
                waited_ms: waited(),
            });
        }
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            let arrived = self.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();
            if let Some(arrival) = self.take(task_id) {
                return Ok(match arrival {
                    Arrival::Steer => WaitResult::UserSteer {
                        waited_ms: waited(),
                    },
                    Arrival::BackgroundTask(id) => WaitResult::Notification {
                        task_ids: vec![id],
                        waited_ms: waited(),
                    },
                });
            }
            tokio::select! {
                _ = &mut arrived => {}
                _ = &mut deadline => return Ok(WaitResult::Timeout { waited_ms: waited() }),
                _ = cancel.cancelled() => {
                    return Ok(WaitResult::Interrupted { waited_ms: waited() });
                }
            }
        }
    }
}

fn wait_definition() -> ToolDefinition {
    let mut definition = definition(
        WAIT_TOOL,
        "Wait until a background task (like exec or agent) finishes, the user or a line of this session sends you a steer, or the timeout elapses. Pass a taskId to wait for one specific task, or omit it to wake on any.",
        json!({
            "type": "object",
            "properties": {
                "taskId": {
                    "type": "string",
                    "description": "Optional background task id to wait for. Omit to wait for any background task."
                },
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": DEFAULT_TIMEOUT_MS,
                    "default": DEFAULT_TIMEOUT_MS,
                    "description": "Wait timeout in milliseconds, from 1 through 300000 inclusive. Default: 300000. This limits the wait call, not the background task."
                }
            }
        }),
    );
    definition.source = ToolSource::Extension;
    definition.risk = Some("session".into());
    definition
}

struct WaitTool(Arc<TreeCommands>);
impl WaitTool {
    async fn run(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, HostError> {
        let task_id = match call.input.get("taskId") {
            None => None,
            Some(Value::String(id)) => Some(id.clone()),
            Some(_) => return Ok(ToolResult::error("wait taskId must be a string.")),
        };
        let timeout_ms = match call.input.get("timeoutMs") {
            None => DEFAULT_TIMEOUT_MS,
            Some(value) => match value.as_f64() {
                Some(ms) if ms.fract() == 0.0 && ms > 0.0 && ms <= DEFAULT_TIMEOUT_MS as f64 => {
                    ms as u64
                }
                Some(ms) if ms.fract() == 0.0 && ms > DEFAULT_TIMEOUT_MS as f64 => {
                    return Ok(ToolResult::error(format!(
                        "wait timeoutMs must be less than or equal to {DEFAULT_TIMEOUT_MS}."
                    )));
                }
                _ => {
                    return Ok(ToolResult::error(
                        "wait timeoutMs must be a positive integer.",
                    ));
                }
            },
        };
        let wait = self
            .0
            .get(&context.branch_id)
            .and_then(|branch| branch.service::<Wait>())
            .map_err(|error| match error {
                AgentError::Reloading(_) => HostError::Cancelled(CAPABILITY_RELOADING.into()),
                error => HostError::failed(error.to_string()),
            })?;
        let Ok(result) = wait
            .arrival(
                task_id.as_deref(),
                Duration::from_millis(timeout_ms),
                cancel,
            )
            .await
        else {
            return Ok(ToolResult::error(AlreadyWaiting.to_string()));
        };
        let content = match &result {
            WaitResult::Notification { task_ids, .. } => {
                format!("wait ended: notification ({})", task_ids.join(", "))
            }
            WaitResult::UserSteer { .. } => "wait ended: steer".into(),
            WaitResult::Interrupted { .. } => "wait ended: interrupted".into(),
            WaitResult::Timeout { .. } => "wait ended: timeout".into(),
        };
        let Value::Object(structured) =
            serde_json::to_value(&result).expect("a wait result is an object")
        else {
            unreachable!("a wait result is an object")
        };
        Ok(ToolResult {
            structured_content: Some(structured),
            ..ToolResult::text(content)
        })
    }
}
impl ToolHandler for WaitTool {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(self.run(call, context, cancel))
    }
}

pub struct WaitPlugin {
    tree: Key<TreeCommands>,
    tools: Key<ToolRegistry>,
}
impl WaitPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            tree: names.key()?,
            tools: names.key()?,
        })
    }
}
impl DynPlugin for WaitPlugin {
    fn name(&self) -> &str {
        "Wait"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase(), self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let tools = ctx.require_at(&self.tools)?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: wait_definition(),
                    handler: Arc::new(WaitTool(tree)),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
pub struct WaitBranchPlugin {
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    wait: Key<Wait>,
}
impl WaitBranchPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
            dispatcher: names.key()?,
            wait: names.key()?,
        })
    }
}
impl DynPlugin for WaitBranchPlugin {
    fn name(&self) -> &str {
        "WaitBranch"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let wait = Arc::new(Wait {
                state: Mutex::new(State::default()),
                arrived: Notify::new(),
            });
            let rebuild = wait.clone();
            let consumer = wait.clone();
            dispatcher
                .consume(
                    ctx,
                    "Wait".into(),
                    ["input/message", "turn/start", "turn/end"]
                        .map(String::from)
                        .to_vec(),
                    Arc::new(move || {
                        let rebuild = rebuild.clone();
                        let timeline = timeline.clone();
                        Box::pin(async move { rebuild.rebuild(&timeline).await })
                    }),
                    Arc::new(move |event| {
                        consumer.consume(&event);
                        Box::pin(async { Ok(()) })
                    }),
                )
                .await?;
            ctx.provide_at(self.wait.clone(), wait)?;
            Ok(Effect::Done)
        })
    }
}

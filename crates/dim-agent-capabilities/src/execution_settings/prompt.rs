//! Model-facing execution settings: snapshots and the one `settings` tool.
//!
//! The App Server's `ExecutionSettings` owns the Settings document and
//! provides `ExecutionSettingsHost`. This module appends a snapshot of the
//! effective switches and registers the single `settings` tool. It does not
//! register a Prompt source.
use super::{ExecutionSettings, ExecutionSettingsHost, SOURCE};
use crate::{
    branch_input::{append_own, append_own_dispatched},
    host::ToolExecutionContext,
    settings::{SettingsChanged, settings_changes},
    tool_registry::{ToolConcurrency, ToolContribution, ToolHandler, ToolRegistry},
};
use dim_agent_runtime::{
    BranchLifecycle, BranchPhase, Dispatcher, Names, OpeningKind, PartRegistry, PartScope, Timeline,
};
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The snapshot source name.
pub const SNAPSHOT_SOURCE: &str = "dim/settings/execution";
/// One retry interval of the snapshot: the published branch's loop waits this
/// long before asking a source again. The opening read is bounded by this and
/// by half the branch's own opening budget, so a source that misses it leaves
/// `pending` set and the loop appends the snapshot after activation instead of
/// holding the opening.
const RETRY: std::time::Duration = std::time::Duration::from_secs(1);
/// The tool name.
const TOOL: &str = "settings";
/// The Session plugin name.
pub const SESSION_PART: &str = "SettingsTool";
/// The branch plugin name.
pub const BRANCH_PART: &str = "SettingsSnapshotBranch";

/// Registers the one settings tool and the settings snapshot branch for the
/// process plugin that owns `ExecutionSettingsHost`.
pub fn register_parts(ctx: &Ctx, parts: &Arc<PartRegistry>) -> Result<(), Error> {
    parts.register(ctx, PartScope::Session, SESSION_PART, |mount| {
        Ok(Some(Box::new(SettingsTool::new(mount.names)?)))
    })?;
    parts.register(ctx, PartScope::Branch, BRANCH_PART, |mount| {
        Ok(Some(Box::new(SettingsSnapshotBranch::new(mount.names)?)))
    })
}

fn body(settings: ExecutionSettings) -> String {
    if settings.sandbox {
        format!(
            "Execution sandbox: on; network: {}. New processes only; a running shell keeps its original boundary.",
            if settings.network { "on" } else { "off" }
        )
    } else {
        "Execution sandbox: off. New processes run with the host's normal access.".into()
    }
}

fn snapshot(settings: ExecutionSettings) -> String {
    format!(
        "<dim-state source=\"{SNAPSHOT_SOURCE}\">{}</dim-state>",
        body(settings)
    )
}

fn input(text: String) -> InputMessageData {
    InputMessageData {
        blocks: vec![InputBlock::Raw {
            text,
            key: None,
            retire: false,
        }],
        ..InputMessageData::default()
    }
}

fn settings_view(settings: ExecutionSettings) -> Value {
    json!({
        "settings": [
            {
                "name": "execution.sandbox",
                "description": "Run shell commands inside the sandbox.",
                "type": "boolean",
                "value": settings.sandbox,
                "writable": false
            },
            {
                "name": "execution.network",
                "description": "Allow network access for new processes. Only meaningful when the sandbox is on.",
                "type": "boolean",
                "value": settings.network,
                "writable": false
            }
        ]
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    op: String,
    name: Option<String>,
}

struct SettingsHandler {
    host: Arc<ExecutionSettingsHost>,
}
impl SettingsHandler {
    async fn answer(&self, call: &ToolCall, cancel: CancellationToken) -> ToolResult {
        let arguments: Arguments = match serde_json::from_value(Value::Object(call.input.clone())) {
            Ok(arguments) => arguments,
            Err(error) => return ToolResult::error(format!("invalid settings arguments: {error}")),
        };
        let settings = match self.host.read(cancel).await {
            Ok(settings) => settings,
            Err(error) => return ToolResult::error(format!("execution settings: {error}")),
        };
        match arguments.op.as_str() {
            "list" => ToolResult::text(settings_view(settings).to_string()),
            "get" => {
                let Some(name) = arguments.name else {
                    return ToolResult::error("settings get requires name");
                };
                let view = settings_view(settings);
                let Some(setting) = view["settings"]
                    .as_array()
                    .and_then(|settings| settings.iter().find(|item| item["name"] == name))
                else {
                    return ToolResult::error(format!("unknown setting {name}; call list"));
                };
                ToolResult::text(setting.to_string())
            }
            other => ToolResult::error(format!("unknown settings op {other}; use list or get")),
        }
    }
}
impl ToolHandler for SettingsHandler {
    /// Both operations read the user's settings.
    fn concurrency(&self, _: &ToolCall) -> ToolConcurrency {
        ToolConcurrency::Concurrent
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> crate::host::HostFuture<'_, ToolResult> {
        Box::pin(async move { Ok(self.answer(&call, cancel).await) })
    }
}

fn definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL.into(),
        description: "Read the user's settings. Call op=list first to see each setting's name, meaning, current value, type, and whether it is writable. op=get reads one setting. Never guess a setting name.".into(),
        input_schema: serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["list", "get"],
                    "description": "list: show available settings. get: read one."
                },
                "name": {
                    "type": "string",
                    "description": "A setting name returned by list."
                }
            },
            "required": ["op"],
            "additionalProperties": false
        }))
        .expect("valid settings schema"),
        source: ToolSource::Extension,
        risk: Some("session".into()),
        metadata: None,
        file_changes: None,
    }
}

struct SettingsTool {
    host: Key<ExecutionSettingsHost>,
    tools: Key<ToolRegistry>,
}
impl SettingsTool {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            host: names.key()?,
            tools: names.key()?,
        })
    }
}
impl DynPlugin for SettingsTool {
    fn name(&self) -> &str {
        SESSION_PART
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.host.erase(), self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let host = ctx.require_at(&self.host)?;
            let tools = ctx.require_at(&self.tools)?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: definition(),
                    handler: Arc::new(SettingsHandler { host }),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

struct SettingsSnapshotBranch {
    host: Key<ExecutionSettingsHost>,
    timeline: Key<Timeline>,
    phase: Key<BranchLifecycle>,
    dispatcher: Key<Dispatcher>,
}
impl SettingsSnapshotBranch {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            host: names.key()?,
            timeline: names.key()?,
            phase: names.key()?,
            dispatcher: names.key()?,
        })
    }
}
impl DynPlugin for SettingsSnapshotBranch {
    fn name(&self) -> &str {
        BRANCH_PART
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.host.erase(),
            self.timeline.erase(),
            self.phase.erase(),
            self.dispatcher.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let host = ctx.require_at(&self.host)?;
            let timeline = ctx.require_at(&self.timeline)?;
            let phase = ctx.require_at(&self.phase)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let wake = Arc::new(Notify::new());
            let listener_wake = wake.clone();
            ctx.listen::<SettingsChanged>()
                .name(settings_changes(SOURCE))
                .on(move |_, _| {
                    listener_wake.notify_one();
                    Box::pin(async { Ok(None) })
                })?;

            let lifecycle = ctx.cancellation_token();
            let tasks = TaskTracker::new();
            let shared = Arc::new(std::sync::Mutex::new((None, true)));
            let opening_budget = phase.opening_budget();
            if phase.phase() == BranchPhase::Loading {
                let host = host.clone();
                let timeline = timeline.clone();
                let dispatcher = dispatcher.clone();
                let lifecycle = lifecycle.clone();
                let shared = shared.clone();
                let registered = phase.admit_opening_with(Box::new(move |opening| {
                    Box::pin(async move {
                        // A child line's identity input is written after the
                        // opening seals, so this receipt leaves the snapshot to
                        // the published line instead of appending ahead of it.
                        if opening == OpeningKind::Child {
                            return Ok(());
                        }
                        // A retired part's receipt stays registered while the
                        // opening still collects receipts, so it checks its own
                        // generation before reading and again before the
                        // append.
                        if lifecycle.is_cancelled() {
                            return Ok(());
                        }
                        // An opening fails after its inactivity budget passes
                        // without progress; half of it leaves the receipt room
                        // to append and dispatch what it read.
                        let bound = RETRY.min(opening_budget / 2);
                        let read = tokio::time::timeout(bound, host.read(lifecycle.clone())).await;
                        match read {
                            Ok(Ok(settings)) => {
                                let current = snapshot(settings);
                                if lifecycle.is_cancelled() {
                                    return Ok(());
                                }
                                match append_own_dispatched(
                                    &timeline,
                                    &dispatcher,
                                    input(current.clone()),
                                )
                                .await
                                {
                                    Ok(_) => *shared.lock().unwrap() = (Some(current), false),
                                    Err(error) => {
                                        tracing::warn!(
                                            %error,
                                            "execution settings were not appended"
                                        );
                                    }
                                }
                            }
                            Ok(Err(error)) => {
                                tracing::warn!(%error, "execution settings were not read");
                            }
                            Err(_) => {
                                tracing::warn!("execution settings were not read in time");
                            }
                        }
                        Ok(())
                    })
                }));
                if let Err(error) = registered {
                    tracing::warn!(%error, "the opening refused the execution settings");
                }
            }
            tasks.spawn(async move {
                tokio::select! {
                    _ = lifecycle.cancelled() => return,
                    active = phase.activated() => if !active { return },
                }
                let (mut last, mut pending) = {
                    let shared = shared.lock().unwrap();
                    (shared.0.clone(), shared.1)
                };
                loop {
                    if pending {
                        let read = tokio::select! {
                            _ = phase.closing() => return,
                            result = host.read(lifecycle.child_token()) => result,
                        };
                        match read {
                            Ok(settings) => {
                                let current = snapshot(settings);
                                if last.as_deref() == Some(current.as_str()) {
                                    pending = false;
                                } else if lifecycle.is_cancelled()
                                    || phase.phase() >= BranchPhase::Closing
                                {
                                    return;
                                } else {
                                    match append_own(&timeline, input(current.clone()), Vec::new())
                                    {
                                        Ok(_) => {
                                            last = Some(current);
                                            pending = false;
                                        }
                                        Err(error) => {
                                            tracing::warn!(
                                                %error,
                                                "execution settings were not appended"
                                            );
                                            pending = true;
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "execution settings were not read");
                                pending = true;
                            }
                        }
                    }
                    tokio::select! {
                        _ = lifecycle.cancelled() => return,
                        _ = phase.closing() => return,
                        _ = wake.notified() => pending = true,
                        _ = tokio::time::sleep(RETRY), if pending => {}
                    }
                }
            });
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

    fn call(input: Value) -> ToolCall {
        ToolCall {
            id: "call".into(),
            name: TOOL.into(),
            input: input.as_object().expect("object").clone(),
        }
    }

    fn handler() -> SettingsHandler {
        SettingsHandler {
            host: Arc::new(ExecutionSettingsHost::fixed(ExecutionSettings {
                sandbox: true,
                network: false,
            })),
        }
    }

    #[tokio::test]
    async fn list_names_both_switches() {
        let result = handler()
            .answer(&call(json!({"op": "list"})), CancellationToken::new())
            .await;
        let ToolContent::Text(text) = result.content else {
            panic!("text result");
        };
        assert!(text.contains("execution.sandbox"));
        assert!(text.contains("execution.network"));
        assert!(text.contains("\"writable\":false"));
    }

    #[tokio::test]
    async fn get_returns_one_setting_and_rejects_unknown_names() {
        let result = handler()
            .answer(
                &call(json!({"op": "get", "name": "execution.sandbox"})),
                CancellationToken::new(),
            )
            .await;
        let ToolContent::Text(text) = result.content else {
            panic!("text result");
        };
        assert!(text.contains("\"value\":true"));

        let result = handler()
            .answer(
                &call(json!({"op": "get", "name": "execution.nope"})),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.is_error, Some(true));
    }
}

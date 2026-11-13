//! The Session part: the `schedule` tool and the `session.wakeup` methods.
//! The handler refuses non-interactive Sessions and Collaboration members
//! before it calls the process service ([design §1.1, A.2, §10](../../../docs/design/plugins/Scheduler.md)).
use crate::{
    document::MAX_PROMPT_BYTES,
    loops::{self, Loops, StartRequest},
    methods::{Caller, ScheduleJobs, ToolRequest},
    wakeups::{WakeupRequest, Wakeups},
};
use dim_agent_capabilities::{
    host::{HostFuture, ToolExecutionContext},
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params, reply},
    prompt_guide::{GuideEntries, GuideEntry, GuideScope},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry, definition},
};
use dim_agent_runtime::{CollaborationRole, Names, SessionConfig, SessionIdentity};
use dim_agent_types::{ToolCall, ToolDefinition, ToolResult, ToolSource};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) const DESCRIPTION: &str = "Manage scheduled jobs across this host, including other workspaces. A job delivers its saved prompt to the branch that saved it, on its schedule. A job saved by a client may launch a new Session instead. A Session may save at most 16 jobs.";

const UNAVAILABLE: &str = "schedule is available only in interactive leader Sessions.";
const WAKEUP_UNAVAILABLE: &str = "wakeup is available only in interactive leader Sessions.";
const UNLOADING_BEFORE_CALL: &str = "tool provider generation is unloading; this call was not made. The tool is offered again once its provider is active; a later request uses the current provider.";
const UNLOADING_AFTER_CALL: &str = "tool provider generation is unloading; this call may already have taken effect. Check the operation's state before repeating it with the current provider.";
const CANCELLED_BEFORE_HANDOVER: &str = "schedule: cancelled before handover";
const WAKEUP_CANCELLED_BEFORE_HANDOVER: &str = "wakeup: cancelled before handover";
/// The `dim-scheduler` guide entry (design §5.2), verbatim.
const ENTRY: &str = "<dim-scheduler kind=\"scheduled-run\" job=\"nightly-build\" run=\"run-7f2\">: a scheduled job's saved prompt, delivered when the job runs, and <dim-scheduler kind=\"wakeup\">: a wakeup's prompt, delivered once at its set time as one turn on the same Session. job names the job on a scheduled run and run names the run. In the scheduled-run tag the body is the job's name, and the message after the tag is the prompt saved for the job: the request this run carries out, in the operator's channel. The wakeup body is the wakeup id. A loop round is one of a loop's prompts, delivered when the loop is due; its attributes name the loop and which round this is, and the loop continues only through the model's `loop` action=next.";

/// The `session.wakeup` namespace (design §10), registered by the Session
/// part; Rpc restores an unloaded Session for each call, so list and cancel
/// keep working when the target is cold.
const WAKEUP_NAMESPACE: &str = "session.wakeup";

/// The `session.loop` namespace (design §12.6), registered by the Session
/// part; Rpc restores an unloaded Session for each call, so list and stop keep
/// working when the target is cold.
const LOOP_NAMESPACE: &str = "session.loop";
const LOOP_UNAVAILABLE: &str = "loop is available only in interactive leader Sessions.";
const LOOP_CANCELLED_BEFORE_HANDOVER: &str = "loop: cancelled before handover";
/// The `loop` tool's declaration (design §12.4). The description sentences
/// that say when to continue and when to stop are policy text.
const LOOP_DESCRIPTION: &str = "Run this Session's work on a rhythm you choose. A loop delivers its prompt as one ordinary input of this branch, marked as that loop's round, and the loop ends unless you arm the next round with action=next.\n- start: begin a loop with delay_seconds until its first round is due (default 300).\n- next: arm the following round after the current one, between 60 and 3600 seconds out; choose short waits while work is moving and long ones when nothing is pending, and stop instead of waiting when the work is done.\n- stop: end the loop now.\n- list: the loops of this branch, with their state, round and next due time.\nA loop ends after 7 days or when it reaches its round limit; a loop whose Session or branch is gone stops.";

fn declaration(description: &str) -> ToolDefinition {
    let mut tool = definition(
        "schedule",
        description,
        json!({
            "type": "object",
            "required": ["action"],
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "create", "update", "delete", "set_enabled", "run_now", "history"]
                },
                "job_id": {"type": "string"},
                "enabled": {"type": "boolean"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                "job": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "name": {"type": "string"},
                        "schedule": {"type": "object"},
                        "prompt": {"type": "string"},
                        "input_mode": {"type": "string", "enum": ["queue", "steer", "append"]}
                    }
                }
            }
        }),
    );
    tool.source = ToolSource::Extension;
    tool.risk = Some("persistent_configuration".into());
    tool
}

/// The `wakeup` tool declaration (design §10).
fn wakeup_declaration() -> ToolDefinition {
    let mut tool = definition(
        "wakeup",
        "Schedule this Session to be woken once at a future time with a prompt. The wakeup fires one turn on the same Session with the given prompt, preserving its history and branch; it can be listed and cancelled from any client, and an expired or cancelled wakeup is never replayed.",
        json!({
            "type": "object",
            "required": ["at", "prompt"],
            "additionalProperties": false,
            "properties": {
                "at": {"type": "string", "description": "The RFC 3339 instant at which the wakeup fires."},
                "prompt": {"type": "string", "description": format!("The text delivered to the Session when it fires, at most {MAX_PROMPT_BYTES} bytes of UTF-8.")}
            }
        }),
    );
    tool.source = ToolSource::Extension;
    tool.risk = Some("persistent_configuration".into());
    tool
}

pub struct SchedulerSession {
    jobs: Key<ScheduleJobs>,
    wakeups: Key<Wakeups>,
    loops: Key<Loops>,
    methods: Key<SessionMethods>,
    tools: Key<ToolRegistry>,
    guide: Key<GuideEntries>,
    config: Key<SessionConfig>,
    identity: Key<SessionIdentity>,
    description: String,
}

impl SchedulerSession {
    pub fn new(names: &Names<'_>, description: String) -> rutis::Result<Self> {
        Ok(Self {
            jobs: names.key()?,
            wakeups: names.key()?,
            loops: names.key()?,
            methods: names.key()?,
            tools: names.key()?,
            guide: names.key()?,
            config: names.key()?,
            identity: names.key()?,
            description,
        })
    }
}

impl DynPlugin for SchedulerSession {
    fn name(&self) -> &str {
        "SchedulerSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.jobs.erase(),
            self.wakeups.erase(),
            self.loops.erase(),
            self.methods.erase(),
            self.tools.erase(),
            self.guide.erase(),
            self.config.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let jobs = ctx.require_at(&self.jobs)?;
            let wakeups = ctx.require_at(&self.wakeups)?;
            let loops = ctx.require_at(&self.loops)?;
            let methods = ctx.require_at(&self.methods)?;
            let tools = ctx.require_at(&self.tools)?;
            let config = ctx.require_at(&self.config)?;
            let identity = ctx.require_at(&self.identity)?;
            let closing = ctx.cancellation_token();
            ctx.require_at(&self.guide)?.register(
                ctx,
                GuideScope::Session,
                GuideEntry {
                    markers: vec!["dim-scheduler".into()],
                    order: 110,
                    text: ENTRY.into(),
                },
            )?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: declaration(&self.description),
                    handler: Arc::new(ScheduleTool {
                        jobs,
                        config: config.clone(),
                        session_id: identity.session_id.clone(),
                        closing: closing.clone(),
                    }),
                },
            )?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: wakeup_declaration(),
                    handler: Arc::new(WakeupTool {
                        wakeups: wakeups.clone(),
                        config: config.clone(),
                        session_id: identity.session_id.clone(),
                        closing: closing.clone(),
                    }),
                },
            )?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: loop_declaration(),
                    handler: Arc::new(LoopTool {
                        loops: loops.clone(),
                        config: config.clone(),
                        session_id: identity.session_id.clone(),
                        closing: closing.clone(),
                    }),
                },
            )?;
            methods.register(
                ctx,
                WAKEUP_NAMESPACE,
                vec![
                    MethodDecl::new("session.wakeup.create", true, Interruption::Drain),
                    MethodDecl::new("session.wakeup.list", false, Interruption::Report),
                    MethodDecl::new("session.wakeup.cancel", true, Interruption::Drain),
                ],
                wakeup_handler(wakeups),
            )?;
            methods.register(
                ctx,
                LOOP_NAMESPACE,
                vec![
                    MethodDecl::new("session.loop.start", true, Interruption::Drain),
                    MethodDecl::new("session.loop.list", false, Interruption::Report),
                    MethodDecl::new("session.loop.stop", true, Interruption::Drain),
                ],
                loop_handler(loops),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateParams {
    session_id: String,
    branch_id: String,
    at: String,
    prompt: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListParams {
    session_id: String,
    #[serde(default)]
    branch_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelParams {
    session_id: String,
    wakeup_id: String,
}

/// The non-empty-string check the neighbouring Session methods use.
fn nonempty(text: &str, field: &str) -> Result<(), RpcError> {
    if text.is_empty() {
        return Err(RpcError::invalid(format!("{field} must not be empty")));
    }
    Ok(())
}

/// The prompt bound shared by the declaration and both entry points (design
/// §10.1, §10.2): an oversized prompt is the caller's error, so it answers
/// `invalid_params` at the wire boundary; the record's own check stays as the
/// storage-side guard. JSON Schema's `maxLength` counts characters, not UTF-8
/// bytes, so the declaration states the bound in prose and the handler stays
/// its single authority.
fn prompt_size(prompt: &str) -> Result<(), String> {
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(format!("prompt is at most {MAX_PROMPT_BYTES} bytes"));
    }
    Ok(())
}

/// Parses the RFC 3339 `at` instant; the canonical UTC millisecond form is
/// stored (A.1's timestamp form).
fn parse_at(at: &str) -> Result<chrono::DateTime<chrono::Utc>, RpcError> {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|instant| instant.with_timezone(&chrono::Utc))
        .map_err(|_| RpcError::invalid("at must be an RFC 3339 timestamp"))
}

/// `loop list` for a model: one line per loop plus the state a next action
/// needs.
fn loop_listing(records: &[loops::Loop]) -> String {
    if records.is_empty() {
        return "loops: none".into();
    }
    let mut text = format!("loops: {}", records.len());
    for record in records {
        let prompt: String = record.prompt.chars().take(120).collect();
        text.push_str(&format!(
            "\n- {} {} round {} next {} {}",
            record.loop_id,
            record.state.name(),
            record.round,
            record.next_at.as_deref().unwrap_or("none"),
            prompt.replace('\n', " "),
        ));
    }
    text
}

/// The `session.loop.*` handler: parses at the wire boundary, forwards to the
/// process owner, and maps its errors.
fn loop_handler(loops: Arc<Loops>) -> dim_agent_capabilities::methods::Handler {
    Arc::new(move |call, _cancel, _progress| {
        let MethodCall {
            method,
            params: value,
            ..
        } = call;
        let loops = loops.clone();
        Box::pin(async move {
            match method.as_str() {
                "session.loop.start" => {
                    let p: LoopStartParams = params(value)?;
                    nonempty(&p.session_id, "sessionId")?;
                    let branch = p.branch_id.as_deref().unwrap_or("main");
                    nonempty(branch, "branchId")?;
                    nonempty(&p.prompt, "prompt")?;
                    prompt_size(&p.prompt).map_err(RpcError::invalid)?;
                    let created = loops
                        .start(StartRequest {
                            session_id: p.session_id,
                            branch_id: branch.to_owned(),
                            prompt: p.prompt,
                            every_seconds: p.every_seconds,
                        })
                        .await?;
                    reply(json!({ "loop": wire_loop(&created) }))
                }
                "session.loop.list" => {
                    let p: LoopListParams = params(value)?;
                    nonempty(&p.session_id, "sessionId")?;
                    let branch = p.branch_id.as_deref().unwrap_or("main");
                    nonempty(branch, "branchId")?;
                    let own = loops.list(&p.session_id, branch).await?;
                    reply(json!({
                        "loops": own.iter().map(wire_loop).collect::<Vec<_>>()
                    }))
                }
                "session.loop.stop" => {
                    let p: LoopStopParams = params(value)?;
                    nonempty(&p.session_id, "sessionId")?;
                    nonempty(&p.loop_id, "loopId")?;
                    let (record, settled) = loops.stop(&p.session_id, &p.loop_id).await?;
                    let mut answer = json!({ "loop": wire_loop(&record) });
                    if !settled {
                        answer["settled"] = json!(false);
                    }
                    reply(answer)
                }
                _ => Err(RpcError::unknown_method()),
            }
        })
    })
}

/// The `loop` tool's declaration (design §12.4). Its sentences on continuing
/// and stopping are policy text.
fn loop_declaration() -> ToolDefinition {
    let mut tool = definition(
        "loop",
        LOOP_DESCRIPTION,
        json!({
            "type": "object",
            "required": ["action"],
            "additionalProperties": false,
            "properties": {
                "action": {"type": "string", "enum": ["start", "next", "stop", "list"]},
                "prompt": {"type": "string", "description": format!("[start] The text each round delivers, at most {MAX_PROMPT_BYTES} bytes of UTF-8.")},
                "delay_seconds": {"type": "integer", "description": "[start, next] Seconds until the round is due; a next delay is clamped to 60..3600."},
                "loop_id": {"type": "string", "description": "[next, stop] The loop to act on; absent, the branch's only loop."}
            }
        }),
    );
    tool.source = ToolSource::Extension;
    tool.risk = Some("persistent_configuration".into());
    tool
}

/// The created record as the wire spells it (design §12.6).
fn wire_loop(record: &loops::Loop) -> Value {
    json!({
        "loopId": record.loop_id,
        "branchId": record.branch_id,
        "state": record.state.name(),
        "round": record.round,
        "maxRounds": record.max_rounds,
        "nextAt": record.next_at,
        "lastRoundAt": record.last_round_at,
        "expiresAt": record.expires_at,
        "stoppedReason": record.stopped_reason,
        "settledAt": record.settled_at,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoopStartParams {
    session_id: String,
    #[serde(default)]
    branch_id: Option<String>,
    prompt: String,
    /// A fixed cadence; absent starts a self-paced loop.
    #[serde(default)]
    every_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoopListParams {
    session_id: String,
    #[serde(default)]
    branch_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoopStopParams {
    session_id: String,
    loop_id: String,
}

/// The created record as the wire spells it (design §10).
fn wire_scheduled(wakeup: &crate::wakeups::Wakeup) -> Value {
    json!({
        "wakeupId": wakeup.wakeup_id,
        "at": wakeup.at,
        "prompt": wakeup.prompt,
        "state": wakeup.state.name(),
    })
}

/// The `session.wakeup.*` handler: parses at the wire boundary, forwards to
/// the process owner, and maps its errors.
fn wakeup_handler(wakeups: Arc<Wakeups>) -> dim_agent_capabilities::methods::Handler {
    Arc::new(move |call, _cancel, _progress| {
        let MethodCall {
            method,
            params: value,
            ..
        } = call;
        let wakeups = wakeups.clone();
        Box::pin(async move {
            match method.as_str() {
                "session.wakeup.create" => {
                    let p: CreateParams = params(value)?;
                    nonempty(&p.session_id, "sessionId")?;
                    nonempty(&p.branch_id, "branchId")?;
                    nonempty(&p.prompt, "prompt")?;
                    prompt_size(&p.prompt).map_err(RpcError::invalid)?;
                    let at = parse_at(&p.at)?;
                    let created = wakeups
                        .create(WakeupRequest {
                            session_id: p.session_id,
                            branch_id: p.branch_id,
                            at,
                            prompt: p.prompt,
                        })
                        .await?;
                    reply(json!({ "wakeupId": created.wakeup_id }))
                }
                "session.wakeup.list" => {
                    let p: ListParams = params(value)?;
                    nonempty(&p.session_id, "sessionId")?;
                    let branch_id = p.branch_id.as_deref().unwrap_or("main");
                    nonempty(branch_id, "branchId")?;
                    let own = wakeups.list(&p.session_id, branch_id).await?;
                    reply(json!({
                        "wakeups": own.iter().map(wire_scheduled).collect::<Vec<_>>()
                    }))
                }
                "session.wakeup.cancel" => {
                    let p: CancelParams = params(value)?;
                    nonempty(&p.session_id, "sessionId")?;
                    nonempty(&p.wakeup_id, "wakeupId")?;
                    let (settled, changed) = wakeups.cancel(&p.session_id, &p.wakeup_id).await?;
                    if changed {
                        reply(json!({}))
                    } else {
                        reply(json!({ "state": settled.state.name() }))
                    }
                }
                _ => Err(RpcError::unknown_method()),
            }
        })
    })
}

struct ScheduleTool {
    jobs: Arc<ScheduleJobs>,
    config: Arc<SessionConfig>,
    session_id: String,
    /// Cancelled when this generation starts to unload.
    closing: CancellationToken,
}

impl ToolHandler for ScheduleTool {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_BEFORE_CALL));
            }
            if !self.config.interactive
                || self.config.collaboration_role == CollaborationRole::Member
            {
                return Ok(ToolResult::error(UNAVAILABLE));
            }
            let request: ToolRequest = match serde_json::from_value(Value::Object(call.input)) {
                Ok(request) => request,
                Err(error) => {
                    return Ok(ToolResult::error(format!(
                        "schedule: schedule_invalid\n{error}"
                    )));
                }
            };
            let caller = Caller {
                session_id: self.session_id.clone(),
                branch_id: context.branch_id,
                cwd: self.config.cwd.clone(),
            };
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_BEFORE_CALL));
            }
            if cancel.is_cancelled() {
                return Ok(ToolResult::error(CANCELLED_BEFORE_HANDOVER));
            }
            let reply = self.jobs.call(request, caller).await;
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_AFTER_CALL));
            }
            Ok(match reply {
                Ok(reply) => {
                    let mut result = ToolResult::text(reply.content);
                    result.structured_content = reply.records.as_object().cloned();
                    result
                }
                Err(error) => {
                    ToolResult::error(format!("schedule: {}\n{}", error.code, error.message))
                }
            })
        })
    }
}

/// The model-facing arguments of `wakeup`, exactly as the declaration reads
/// them (design §10); the Session and branch default to the caller.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WakeupArgs {
    at: String,
    prompt: String,
}

struct WakeupTool {
    wakeups: Arc<Wakeups>,
    config: Arc<SessionConfig>,
    session_id: String,
    /// Cancelled when this generation starts to unload.
    closing: CancellationToken,
}

impl ToolHandler for WakeupTool {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_BEFORE_CALL));
            }
            if !self.config.interactive
                || self.config.collaboration_role == CollaborationRole::Member
            {
                return Ok(ToolResult::error(WAKEUP_UNAVAILABLE));
            }
            let args: WakeupArgs = match serde_json::from_value(Value::Object(call.input)) {
                Ok(args) => args,
                Err(error) => {
                    return Ok(ToolResult::error(format!(
                        "wakeup: invalid_params\n{error}"
                    )));
                }
            };
            if args.prompt.is_empty() {
                return Ok(ToolResult::error(
                    "wakeup: invalid_params\nprompt must not be empty",
                ));
            }
            if let Err(message) = prompt_size(&args.prompt) {
                return Ok(ToolResult::error(format!(
                    "wakeup: invalid_params\n{message}"
                )));
            }
            let at = match chrono::DateTime::parse_from_rfc3339(&args.at) {
                Ok(instant) => instant.with_timezone(&chrono::Utc),
                Err(_) => {
                    return Ok(ToolResult::error(
                        "wakeup: invalid_params\nat must be an RFC 3339 timestamp",
                    ));
                }
            };
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_BEFORE_CALL));
            }
            if cancel.is_cancelled() {
                return Ok(ToolResult::error(WAKEUP_CANCELLED_BEFORE_HANDOVER));
            }
            let created = self
                .wakeups
                .create(WakeupRequest {
                    session_id: self.session_id.clone(),
                    branch_id: context.branch_id,
                    at,
                    prompt: args.prompt,
                })
                .await;
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_AFTER_CALL));
            }
            Ok(match created {
                Ok(wakeup) => {
                    let record = wire_scheduled(&wakeup);
                    let mut result = ToolResult::text(format!("wakeup: created\n{record}"));
                    result.structured_content = record.as_object().cloned();
                    result
                }
                Err(error) => {
                    ToolResult::error(format!("wakeup: {}\n{}", error.code, error.message))
                }
            })
        })
    }
}

/// The model-facing arguments of `loop`, exactly as the declaration reads them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopArgs {
    action: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    delay_seconds: Option<i64>,
    #[serde(default)]
    every_seconds: Option<u64>,
    #[serde(default)]
    loop_id: Option<String>,
}

struct LoopTool {
    loops: Arc<Loops>,
    config: Arc<SessionConfig>,
    session_id: String,
    /// Cancelled when this generation starts to unload.
    closing: CancellationToken,
}

impl LoopTool {
    /// The loop a `next` or `stop` names: the given id, or the branch's only
    /// live loop.
    async fn target(&self, branch_id: &str, loop_id: Option<&str>) -> Result<String, String> {
        if let Some(id) = loop_id {
            return Ok(id.to_owned());
        }
        let own = self
            .loops
            .list(&self.session_id, branch_id)
            .await
            .map_err(|error| format!("loop: {}\n{}", error.code, error.message))?;
        let mut live: Vec<&loops::Loop> =
            own.iter().filter(|record| record.state.is_live()).collect();
        match live.len() {
            1 => Ok(live.remove(0).loop_id.clone()),
            0 => Err("loop: loop_not_found\nthis branch has no live loop".into()),
            _ => Err(
                "loop: loop_invalid\nthis branch has several live loops; name one with loop_id"
                    .into(),
            ),
        }
    }
}

impl ToolHandler for LoopTool {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_BEFORE_CALL));
            }
            if !self.config.interactive
                || self.config.collaboration_role == CollaborationRole::Member
            {
                return Ok(ToolResult::error(LOOP_UNAVAILABLE));
            }
            let args: LoopArgs = match serde_json::from_value(Value::Object(call.input)) {
                Ok(args) => args,
                Err(error) => {
                    return Ok(ToolResult::error(format!("loop: loop_invalid\n{error}")));
                }
            };
            if cancel.is_cancelled() {
                return Ok(ToolResult::error(LOOP_CANCELLED_BEFORE_HANDOVER));
            }
            let branch_id = context.branch_id;
            let outcome = match args.action.as_str() {
                "start" => {
                    let Some(prompt) = args.prompt.filter(|prompt| !prompt.is_empty()) else {
                        return Ok(ToolResult::error(
                            "loop: loop_invalid\nstart requires a prompt",
                        ));
                    };
                    if let Err(message) = prompt_size(&prompt) {
                        return Ok(ToolResult::error(format!("loop: loop_invalid\n{message}")));
                    }
                    self.loops
                        .start(StartRequest {
                            session_id: self.session_id.clone(),
                            branch_id: branch_id.clone(),
                            prompt,
                            every_seconds: args.every_seconds,
                        })
                        .await
                        .map(|record| {
                            format!(
                                "loop: started\n{}\nThe first round is due now; arm the following one with action=next.",
                                wire_loop(&record)
                            )
                        })
                        .map_err(|error| format!("loop: {}\n{}", error.code, error.message))
                }
                "next" => {
                    let loop_id = match self.target(&branch_id, args.loop_id.as_deref()).await {
                        Ok(id) => id,
                        Err(message) => return Ok(ToolResult::error(message)),
                    };
                    let delay = args.delay_seconds.unwrap_or(300);
                    self.loops
                        .next(&self.session_id, &loop_id, delay)
                        .await
                        .map(|record| format!("loop: next\n{}", wire_loop(&record)))
                        .map_err(|error| format!("loop: {}\n{}", error.code, error.message))
                }
                "stop" => {
                    let loop_id = match self.target(&branch_id, args.loop_id.as_deref()).await {
                        Ok(id) => id,
                        Err(message) => return Ok(ToolResult::error(message)),
                    };
                    self.loops
                        .stop(&self.session_id, &loop_id)
                        .await
                        .map(|(record, _)| format!("loop: stopped\n{}", wire_loop(&record)))
                        .map_err(|error| format!("loop: {}\n{}", error.code, error.message))
                }
                "list" => self
                    .loops
                    .list(&self.session_id, &branch_id)
                    .await
                    .map(|own| format!("loop: list\n{}", loop_listing(&own)))
                    .map_err(|error| format!("loop: {}\n{}", error.code, error.message)),
                other => Err(format!(
                    "loop: loop_invalid\nunknown action {other}; use start, next, stop or list"
                )),
            };
            if self.closing.is_cancelled() {
                return Ok(ToolResult::error(UNLOADING_AFTER_CALL));
            }
            Ok(match outcome {
                Ok(text) => ToolResult::text(text),
                Err(message) => ToolResult::error(message),
            })
        })
    }
}

//! The Session part: the `schedule` tool and the `session.wakeup` methods.
//! The handler refuses non-interactive Sessions and Collaboration members
//! before it calls the process service ([design §1.1, A.2, §10](../../../docs/design/plugins/Scheduler.md)).
use crate::{
    document::MAX_PROMPT_BYTES,
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
const ENTRY: &str = "<dim-scheduler kind=\"scheduled-run\" job=\"nightly-build\" run=\"run-7f2\">: a scheduled job's saved prompt, delivered when the job runs, and <dim-scheduler kind=\"wakeup\">: a wakeup's prompt, delivered once at its set time as one turn on the same Session. job names the job on a scheduled run and run names the run. In the scheduled-run tag the body is the job's name, and the message after the tag is the prompt saved for the job: the request this run carries out, in the operator's channel. The wakeup body is the wakeup id.";

/// The `session.wakeup` namespace (design §10), registered by the Session
/// part; Rpc restores an unloaded Session for each call, so list and cancel
/// keep working when the target is cold.
const WAKEUP_NAMESPACE: &str = "session.wakeup";

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

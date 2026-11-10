use crate::{
    evidence::{ErrorReference, Snapshot},
    render::{self, InstructionsStatus, Reviewed},
};
use dim_agent_capabilities::{
    methods::{Caller, Interruption, MethodCall, MethodDecl, MethodTable, RpcError, params, reply},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{SessionHold, SessionLifecycle};
use dim_agent_types::ModelId;
use rutis::{Ctx, host::CtxHost};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::{pending, ready},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::{
    sync::{CancellationToken, DropGuard},
    task::TaskTracker,
};
use uuid::Uuid;

const LIFETIME: Duration = Duration::from_secs(5 * 60);
const DRAFT_LIMIT: usize = 16;
const INPUT_LIMIT: usize = 1024;

pub(super) struct Session {
    pub(super) lifecycle: Arc<SessionLifecycle>,
    pub(super) tree: Arc<TreeCommands>,
}

pub(super) fn register(
    ctx: &Ctx,
    methods: &MethodTable,
    namespace: &str,
    session: Option<Session>,
) -> rutis::Result {
    let owner = Arc::new(Reports {
        session,
        drafts: Mutex::default(),
        stop: ctx.cancellation_token(),
        tasks: TaskTracker::new(),
    });
    let cleanup = owner.clone();
    ctx.effect(move || async move {
        cleanup.stop.cancel();
        cleanup.tasks.close();
        cleanup.tasks.wait().await;
        cleanup.drafts.lock().unwrap().clear();
        Ok(())
    })?;

    methods.register(
        ctx,
        namespace,
        ["prepare", "plan", "generate", "cancel"]
            .into_iter()
            .map(|method| {
                MethodDecl::new(&format!("{namespace}.{method}"), true, Interruption::Report)
            })
            .collect(),
        Arc::new(move |mut call, cancel, _| {
            let owner = owner.clone();
            Box::pin(async move {
                let caller = call.origin.caller.take().ok_or_else(|| {
                    RpcError::invalid("Report methods require a client connection")
                })?;
                let interrupted = async {
                    tokio::select! {
                        _ = caller.closed() => {},
                        _ = owner.stop.cancelled() => {},
                        _ = owner.session_closing() => {},
                        _ = cancel.cancelled() => {},
                    }
                };
                tokio::pin!(interrupted);
                let result = tokio::select! {
                    biased;
                    _ = &mut interrupted => return Err(cancelled()),
                    result = owner.call(call, &caller) => result?,
                };
                tokio::select! {
                    biased;
                    _ = interrupted => Err(cancelled()),
                    result = ready(result) => Ok(result),
                }
            })
        }),
    )
}

struct Reports {
    session: Option<Session>,
    drafts: Mutex<HashMap<String, Draft>>,
    stop: CancellationToken,
    tasks: TaskTracker,
}

struct Draft {
    id: Uuid,
    snapshot: Snapshot,
    plan: Option<Plan>,
    running: Option<GenerationRun>,
    expires: Instant,
    _drop: DropGuard,
    _hold: Option<SessionHold>,
}

struct GenerationRun {
    plan_id: Uuid,
    cancelled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    error: ErrorReference,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Generation {
    Local,
    Model { model: ModelId },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlanRequest {
    report_id: Uuid,
    generation: Generation,
    redact: bool,
    instructions: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Plan {
    plan_id: Uuid,
    evidence: Reviewed,
    instructions_status: InstructionsStatus,
    generation: Generation,
}

impl Plan {
    fn local(snapshot: &Snapshot, redact: bool, instructions: &str) -> Self {
        let (evidence, instructions_status) = render::review(snapshot, redact, instructions);
        Self {
            plan_id: Uuid::new_v4(),
            evidence,
            instructions_status,
            generation: Generation::Local,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Generate {
    report_id: Uuid,
    plan_id: Uuid,
}

#[derive(Deserialize)]
#[serde(
    tag = "target",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Cancel {
    Generation { report_id: Uuid, plan_id: Uuid },
    Report { report_id: Uuid },
}

impl Reports {
    async fn session_closing(&self) {
        match &self.session {
            Some(session) => session.lifecycle.closing().await,
            None => pending().await,
        }
    }

    async fn call(self: &Arc<Self>, call: MethodCall, caller: &Caller) -> Result<Value, RpcError> {
        let MethodCall {
            method,
            params: mut value,
            ..
        } = call;
        if self.session.is_some() {
            value
                .as_object_mut()
                .ok_or_else(|| RpcError::invalid("Expected report parameters"))?
                .remove("sessionId");
        }

        match method.rsplit('.').next() {
            Some("prepare") => self.prepare(caller, value).await,
            Some("plan") => self.plan(caller.id(), value),
            Some("generate") => self.generate(caller.id(), params(value)?),
            Some("cancel") => self.cancel(caller.id(), params(value)?),
            _ => unreachable!("registered report method"),
        }
    }

    async fn prepare(self: &Arc<Self>, caller: &Caller, value: Value) -> Result<Value, RpcError> {
        if let Some(observed) = value.pointer("/error/observed")
            && observed.to_string().len() > INPUT_LIMIT
        {
            return Err(RpcError::invalid("Observed error exceeds 1 KiB"));
        }
        let Prepare { error } = params(value)?;
        let (snapshot, default_model) = error
            .capture(self.session.as_ref().map(|session| &*session.tree))
            .await?;
        let plan = Plan::local(&snapshot, true, "");
        let id = Uuid::new_v4();
        let mut answer = json!({"reportId": id, "plan": plan});
        if let Some(default_model) = default_model {
            answer["defaultModel"] = reply(default_model)?;
        }

        let expires = Instant::now() + LIFETIME;
        let hold = self
            .session
            .as_ref()
            .map(|session| session.lifecycle.take_hold().ok_or_else(cancelled))
            .transpose()?;
        let token = CancellationToken::new();
        let caller_id = caller.id().to_owned();
        let mut drafts = self.drafts.lock().unwrap();
        drafts.retain(|_, draft| draft.expires > Instant::now());
        if drafts.len() >= DRAFT_LIMIT && !drafts.contains_key(&caller_id) {
            return Err(RpcError::service("report_busy", "Too many open reports"));
        }
        if drafts
            .get(&caller_id)
            .is_some_and(|draft| draft.running.is_some())
        {
            return Err(busy());
        }
        drafts.insert(
            caller_id.clone(),
            Draft {
                id,
                snapshot,
                plan: Some(plan),
                running: None,
                expires,
                _drop: token.clone().drop_guard(),
                _hold: hold,
            },
        );
        let owner = self.clone();
        let closed = caller.closed();
        self.tasks.spawn(async move {
            tokio::select! {
                _ = token.cancelled() => return,
                _ = owner.stop.cancelled() => {},
                _ = owner.session_closing() => {},
                _ = closed => {},
                _ = tokio::time::sleep_until(expires) => {},
            }
            let mut drafts = owner.drafts.lock().unwrap();
            if drafts.get(&caller_id).is_some_and(|draft| draft.id == id) {
                drafts.remove(&caller_id);
            }
        });
        Ok(answer)
    }

    fn plan(&self, caller: &str, value: Value) -> Result<Value, RpcError> {
        if let Some(model) = value
            .pointer("/generation/model")
            .and_then(Value::as_object)
            && model.len() != 2
        {
            return Err(RpcError::invalid("Invalid model identity"));
        }
        let request: PlanRequest = params(value)?;
        let instructions = request.instructions.as_deref().unwrap_or_default();
        if instructions.len() > INPUT_LIMIT {
            return Err(RpcError::invalid("Instructions exceed 1 KiB"));
        }
        let mut drafts = self.drafts.lock().unwrap();
        let draft = get(&mut drafts, caller, request.report_id)?;
        if let Generation::Model { model } = request.generation {
            if model.provider_id.is_empty() || model.model_id.is_empty() {
                return Err(RpcError::invalid("Invalid model identity"));
            }
            return Err(RpcError::service(
                "report_model_unavailable",
                "Model-assisted reports are unavailable",
            ));
        }
        if draft.running.is_some() {
            return Err(busy());
        }
        let plan = Plan::local(&draft.snapshot, request.redact, instructions);
        let response = reply(&plan)?;
        draft.plan = Some(plan);
        Ok(response)
    }

    fn generate(&self, caller: &str, request: Generate) -> Result<Value, RpcError> {
        let plan = {
            let mut drafts = self.drafts.lock().unwrap();
            let draft = get(&mut drafts, caller, request.report_id)?;
            if draft.running.is_some() {
                return Err(busy());
            }
            let plan = draft
                .plan
                .take_if(|plan| plan.plan_id == request.plan_id)
                .ok_or_else(|| {
                    RpcError::service(
                        "report_plan_stale",
                        "The report plan has changed or was consumed",
                    )
                })?;
            draft.running = Some(GenerationRun {
                plan_id: plan.plan_id,
                cancelled: false,
            });
            plan
        };
        let response = plan.evidence.generate();
        let mut drafts = self.drafts.lock().unwrap();
        let draft = get(&mut drafts, caller, request.report_id)?;
        let run = draft
            .running
            .take()
            .expect("the running report owns this draft");
        if run.cancelled {
            return Err(cancelled());
        }
        let response = response?;
        drafts.remove(caller);
        Ok(response)
    }

    fn cancel(&self, caller: &str, request: Cancel) -> Result<Value, RpcError> {
        let mut drafts = self.drafts.lock().unwrap();
        let cancelled = match request {
            Cancel::Generation { report_id, plan_id } => {
                let draft = get(&mut drafts, caller, report_id)?;
                match draft.running.as_mut() {
                    Some(run) if run.plan_id == plan_id && !run.cancelled => {
                        run.cancelled = true;
                        true
                    }
                    _ => false,
                }
            }
            Cancel::Report { report_id } => {
                get(&mut drafts, caller, report_id)?;
                drafts.remove(caller);
                true
            }
        };
        Ok(json!({"cancelled": cancelled}))
    }
}

fn get<'a>(
    drafts: &'a mut HashMap<String, Draft>,
    caller: &str,
    id: Uuid,
) -> Result<&'a mut Draft, RpcError> {
    drafts
        .get_mut(caller)
        .filter(|draft| draft.id == id && draft.expires > Instant::now())
        .ok_or_else(|| RpcError::service("report_not_found", "Report draft is unavailable"))
}

fn busy() -> RpcError {
    RpcError::service("report_busy", "A report generation is settling")
}

fn cancelled() -> RpcError {
    RpcError::service("report_cancelled", "The report call was cancelled")
}

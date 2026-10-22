//! Questions ([design](../../../docs/design/plugins/Questions.md)): the model
//! asks the user through the `ask_user` tool and waits; a client answers with
//! the Session method `session.question.respond`. The question is the pending
//! tool call and the answer is its `ToolResult`, which ToolExecutor commits in
//! the batch's `step/tool-result`, so Questions writes no events.
//!
//! One Session part provides both entries and keeps, in memory, the waiters of
//! the Session's running `ask_user` calls keyed by `(branchId, requestId,
//! callId)`: call ids come from model output and may repeat in later model
//! requests, and `requestId`, the `request_id` of the call's `step/tool-call`,
//! tells those calls apart. An entry lives exactly as long as its handler
//! waits. Whoever removes it under the lock decides the call's output, so an
//! answer and a cancellation never both win. Interrupt, close and a ToolExecutor reload reach a waiting call
//! through the cancel token ToolExecutor gives it; this part's own unload
//! drops its sender.
use dim_agent_capabilities::{
    host::{HostError, HostFuture, ToolExecutionContext},
    methods::{Interruption, MethodDecl, RpcError, SessionMethods, params},
    tool_registry::{ToolContribution, ToolHandler, definition},
};
use dim_agent_runtime::{
    AgentBuilder, CAPABILITY_RELOADING, Capability, InSession, KeysOf, SessionConfig, SessionScope,
    scoped,
};
use dim_agent_types::{ToolCall, ToolDefinition, ToolResult, ToolSource};
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const TOOL: &str = "ask_user";
/// The `sessionMethods` namespace Questions provides.
const NAMESPACE: &str = "session.question";
const RESPOND: &str = "session.question.respond";

const MAX_QUESTIONS: usize = 4;
const OPTIONS: std::ops::RangeInclusive<usize> = 2..=6;
const MAX_HEADER_CHARS: usize = 40;

const DECLINED: &str = "User declined to answer. Proceed without these answers.";
const NON_INTERACTIVE: &str =
    "No one is available to answer questions in this session. Proceed without asking.";
const CANCELLED: &str = "The question was cancelled before the user answered.";

/// Records the tool name and the namespace and installs the Session part. The
/// tool is declared in a Session that lists `ask_user` in
/// `SessionConfig.tools`; installation never reads `interactive`.
pub fn install(builder: &mut AgentBuilder) {
    builder
        .session_tool(TOOL)
        .session_methods(NAMESPACE)
        .session_plugin(QuestionsPlugin::new);
}

/// One question of a call, after the boundary checks of A.1.
struct Question {
    id: String,
    question: String,
    labels: Vec<String>,
    multi_select: bool,
    allow_custom: bool,
}

/// The tool input as the model writes it (A.1).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    questions: Vec<QuestionInput>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QuestionInput {
    id: Option<String>,
    header: Option<String>,
    question: String,
    options: Vec<OptionInput>,
    #[serde(default)]
    multi_select: bool,
    #[serde(default = "allowed")]
    allow_custom: bool,
}
fn allowed() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionInput {
    label: String,
    #[serde(rename = "description")]
    _description: Option<String>,
}

/// Checks the tool input; the error is the reason the model reads.
fn parse(input: serde_json::Map<String, Value>) -> Result<Vec<Question>, String> {
    let input: Input =
        serde_json::from_value(Value::Object(input)).map_err(|error| error.to_string())?;
    if !(1..=MAX_QUESTIONS).contains(&input.questions.len()) {
        return Err(format!(
            "questions must hold 1 to {MAX_QUESTIONS} questions"
        ));
    }
    let mut ids = HashSet::new();
    input
        .questions
        .into_iter()
        .enumerate()
        .map(|(index, question)| {
            let id = question.id.unwrap_or_else(|| (index + 1).to_string());
            if id.is_empty() || !ids.insert(id.clone()) {
                return Err(format!("question id {id:?} is empty or not unique"));
            }
            if question
                .header
                .is_some_and(|header| header.chars().count() > MAX_HEADER_CHARS)
            {
                return Err(format!(
                    "header of question {id} exceeds {MAX_HEADER_CHARS} characters"
                ));
            }
            if question.question.trim().is_empty() {
                return Err(format!("question {id} has no text"));
            }
            if !OPTIONS.contains(&question.options.len()) {
                return Err(format!(
                    "question {id} must offer {} to {} options",
                    OPTIONS.start(),
                    OPTIONS.end()
                ));
            }
            let mut labels = Vec::with_capacity(question.options.len());
            for option in question.options {
                if option.label.trim().is_empty() || labels.contains(&option.label) {
                    return Err(format!(
                        "option labels of question {id} must be non-empty and unique"
                    ));
                }
                labels.push(option.label);
            }
            Ok(Question {
                id,
                question: question.question,
                labels,
                multi_select: question.multi_select,
                allow_custom: question.allow_custom,
            })
        })
        .collect()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Answer {
    question_id: String,
    selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    custom: Option<String>,
}

/// The output `respond` hands to a waiting call; also its `structuredContent`.
#[derive(Serialize)]
struct Answered {
    answers: Vec<Answer>,
    declined: bool,
}

/// Checks `answers` against the call's questions (A.3) and returns them in
/// question order; the error is the `invalid_params` reason.
fn check(questions: &[Question], mut answers: Vec<Answer>) -> Result<Vec<Answer>, String> {
    if let Some(unknown) = answers
        .iter()
        .find(|answer| !questions.iter().any(|q| q.id == answer.question_id))
    {
        return Err(format!("unknown question id {:?}", unknown.question_id));
    }
    questions
        .iter()
        .map(|question| {
            let id = &question.id;
            let mut matching = answers.iter().filter(|answer| answer.question_id == *id);
            if matching.next().is_none() || matching.next().is_some() {
                return Err(format!("question {id} needs exactly one answer"));
            }
            let index = answers
                .iter()
                .position(|answer| answer.question_id == *id)
                .expect("one answer");
            let answer = answers.swap_remove(index);
            let mut seen = HashSet::new();
            if let Some(label) = answer
                .selected
                .iter()
                .find(|label| !question.labels.contains(label) || !seen.insert(*label))
            {
                return Err(format!(
                    "{label:?} is not an option of question {id} or is selected twice"
                ));
            }
            match &answer.custom {
                Some(_) if !question.allow_custom => {
                    return Err(format!("question {id} does not accept a custom answer"));
                }
                Some(custom) if custom.trim().is_empty() => {
                    return Err(format!("the custom answer of question {id} is empty"));
                }
                _ => {}
            }
            let chosen = answer.selected.len() + usize::from(answer.custom.is_some());
            if chosen == 0 {
                return Err(format!("question {id} has no answer"));
            }
            if chosen > 1 && !question.multi_select {
                return Err(format!("question {id} takes a single answer"));
            }
            Ok(answer)
        })
        .collect()
}

/// The `ToolResult` of an answered or declined call (A.1).
fn result(questions: &[Question], answered: Answered) -> ToolResult {
    let content = if answered.declined {
        DECLINED.to_owned()
    } else {
        let mut text = String::from("User answered:");
        for (index, (question, answer)) in questions.iter().zip(&answered.answers).enumerate() {
            let chosen: Vec<_> = answer
                .selected
                .iter()
                .cloned()
                .chain(
                    answer
                        .custom
                        .iter()
                        .map(|custom| format!("(custom) {custom}")),
                )
                .collect();
            text.push_str(&format!(
                "\n{}. {}\n   -> {}",
                index + 1,
                question.question,
                chosen.join(", ")
            ));
        }
        text
    };
    let Ok(Value::Object(structured)) = serde_json::to_value(&answered) else {
        unreachable!("Answered serializes to an object")
    };
    ToolResult {
        structured_content: Some(structured),
        ..ToolResult::text(content)
    }
}

struct Waiter {
    questions: Arc<[Question]>,
    sender: oneshot::Sender<Answered>,
}

#[derive(Default)]
struct Waiters {
    /// Set by the unload; no call registers afterwards.
    closed: bool,
    /// By `(branch_id, request_id, call_id)`.
    table: HashMap<(String, String, String), Waiter>,
}

struct Questions {
    config: Arc<SessionConfig>,
    waiters: Mutex<Waiters>,
}

impl Questions {
    async fn ask(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, HostError> {
        let questions: Arc<[Question]> = match parse(call.input) {
            Ok(questions) => questions.into(),
            Err(reason) => {
                return Ok(ToolResult::error(format!(
                    "Invalid ask_user input: {reason}"
                )));
            }
        };
        if !self.config.interactive {
            return Ok(ToolResult::error(NON_INTERACTIVE));
        }
        // A token that has already fired registers nothing (Appendix B).
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled(CANCELLED.into()));
        }
        let key = (context.branch_id, context.request_id, call.id);
        let (sender, mut answer) = oneshot::channel();
        {
            let mut waiters = self.waiters.lock().unwrap();
            if waiters.closed {
                return Err(HostError::Cancelled(CAPABILITY_RELOADING.into()));
            }
            if waiters.table.contains_key(&key) {
                return Ok(ToolResult::error(format!(
                    "Invalid ask_user input: duplicate call id {}",
                    key.2
                )));
            }
            waiters.table.insert(
                key.clone(),
                Waiter {
                    questions: questions.clone(),
                    sender,
                },
            );
        }
        let answered = tokio::select! {
            biased;
            answered = &mut answer => answered,
            _ = cancel.cancelled() => {
                if self.waiters.lock().unwrap().table.remove(&key).is_some() {
                    return Err(HostError::Cancelled(CANCELLED.into()));
                }
                // `respond` removed the entry first and sent under the lock,
                // or the unload dropped the sender.
                answer.await
            }
        };
        // A dropped sender is the unload's: the batch is reload-settled.
        let answered = answered.map_err(|_| HostError::Cancelled(CAPABILITY_RELOADING.into()))?;
        Ok(result(&questions, answered))
    }

    fn respond(&self, value: Value) -> Result<Value, RpcError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Params {
            /// Read by Rpc to route the call; the provider serves this Session.
            #[serde(rename = "sessionId")]
            _session_id: String,
            branch_id: String,
            /// The `request_id` of the call's `step/tool-call`.
            request_id: String,
            call_id: String,
            answers: Option<Vec<Answer>>,
            #[serde(default)]
            declined: bool,
        }
        let p: Params = params(value)?;
        let answers = match (p.answers, p.declined) {
            (Some(_), true) => {
                return Err(RpcError::invalid("answers cannot be sent with declined"));
            }
            (None, false) => return Err(RpcError::invalid("answers or declined is required")),
            (answers, _) => answers,
        };
        let mut waiters = self.waiters.lock().unwrap();
        // A response to a settled call never reaches a later call that
        // reuses its call id: their request ids differ.
        let key = (p.branch_id, p.request_id, p.call_id);
        let waiter = waiters.table.get(&key).ok_or_else(|| {
            RpcError::app(
                -32000,
                "question_not_pending",
                "No ask_user call is waiting for this answer",
            )
        })?;
        let answered = match answers {
            Some(answers) => Answered {
                answers: check(&waiter.questions, answers).map_err(RpcError::invalid)?,
                declined: false,
            },
            None => Answered {
                answers: Vec::new(),
                declined: true,
            },
        };
        let waiter = waiters.table.remove(&key).expect("checked entry");
        // Under the lock, so a cancel that finds no entry finds the answer.
        let _ = waiter.sender.send(answered);
        Ok(json!({}))
    }
}

struct AskUser(Arc<Questions>);
impl ToolHandler for AskUser {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(self.0.ask(call, context, cancel))
    }
}

fn ask_user_definition() -> ToolDefinition {
    let option = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["label"],
        "properties": {
            "label": {"type": "string", "description": "The answer as the user sees and picks it; unique in the question."},
            "description": {"type": "string", "description": "Optional short explanation of the option."}
        }
    });
    let question = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["question", "options"],
        "properties": {
            "id": {"type": "string", "description": "Optional id, unique in this call. Default: the question's position, \"1\", \"2\", ..."},
            "header": {"type": "string", "maxLength": MAX_HEADER_CHARS, "description": "Optional short label shown above the question."},
            "question": {"type": "string", "description": "The question, as one complete sentence."},
            "options": {"type": "array", "minItems": OPTIONS.start(), "maxItems": OPTIONS.end(), "items": option},
            "multiSelect": {"type": "boolean", "description": "Whether the user may pick several options. Default: false."},
            "allowCustom": {"type": "boolean", "description": "Whether the user may type an answer of their own. Default: true."}
        }
    });
    let mut definition = definition(
        TOOL,
        "Ask the user one or more multiple-choice questions and wait for the answers. Use it when a decision or a fact only the user can provide blocks the work. The user may pick options, type their own answer where allowed, or decline; the result says which.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["questions"],
            "properties": {
                "questions": {"type": "array", "minItems": 1, "maxItems": MAX_QUESTIONS, "items": question}
            }
        }),
    );
    definition.source = ToolSource::Extension;
    definition.risk = Some("session".into());
    definition
}

/// The Questions Session part: `toolContribution` `ask_user` and
/// `sessionMethods` `session.question`.
pub struct QuestionsPlugin {
    scope: SessionScope,
}
impl QuestionsPlugin {
    pub fn new(scope: &SessionScope) -> Self {
        Self { scope: *scope }
    }
}
impl TypedPlugin for QuestionsPlugin {
    type Deps = (InSession<SessionConfig>,);
    fn name(&self) -> &str {
        "Questions"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InSession(config),): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let questions = Arc::new(Questions {
                config,
                waiters: Mutex::default(),
            });
            let owner = questions.clone();
            ctx.effect(move || {
                Effect::Disposer(Box::new(move || {
                    // Dropping the senders ends every waiting call as reloading.
                    let mut waiters = owner.waiters.lock().unwrap();
                    waiters.closed = true;
                    waiters.table.clear();
                    Ok(())
                }))
            })?;
            ctx.provide_as(
                self.scope.named_key::<ToolContribution>(TOOL),
                Arc::new(ToolContribution {
                    definition: ask_user_definition(),
                    handler: Arc::new(AskUser(questions.clone())),
                }),
            )?;
            SessionMethods::provide(
                ctx,
                &self.scope,
                NAMESPACE,
                vec![MethodDecl::new(RESPOND, true, Interruption::Report)],
                Arc::new(move |_, value, _, _| {
                    let result = questions.respond(value);
                    Box::pin(async move { result })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for QuestionsPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

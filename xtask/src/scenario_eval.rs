//! `scenario-eval`: the scenario runner for policy texts (model-prompts §8.1,
//! §8.2 and §9.8; Timeline branches §12.3). It calls real models, so it is not
//! part of CI: the operator runs it against a model configuration, and the
//! output goes outside the repository.
//!
//! ```text
//! cargo run --locked -p xtask -- scenario-eval --scenarios DIR --text ID
//!     --candidate FILE [--text ID --candidate FILE]... --model PROVIDER/MODEL
//!     --judge PROVIDER/MODEL --runs N --out DIR [--jobs N]
//!     [--model-config FILE]
//! ```
//!
//! Both arms run the shipped assembly: `current` with the product's own texts
//! and `candidate` with each candidate file in place of the text its `--text`
//! id names. Every `--text ID --candidate FILE` pair replaces one text, a
//! pair reads in either order, a text named twice is an error, and the report
//! gives each candidate text's own token cost. The runs are interleaved: for
//! each run index and scenario the `current` run is scheduled before the
//! `candidate` one, so a drift in
//! the endpoint lands on both arms alike, and the records come out in that
//! order whatever order the runs complete in. `--jobs N` (default 1) runs N
//! runs at once; each run keeps its own workspace copy, Dim home, local
//! endpoint and Session, and the schedule and record order are fixed
//! whatever N is. The outcomes can still vary: wall-clock budgets, timed
//! steers and provider contention do not read the job count.
//!
//! The report applies the rule of model-prompts §8.1. One comparison is the
//! candidate arm's loss against the current arm on one rate: a regression
//! when the loss exceeds `δ = 0.05` and a one-sided mid-p Fisher exact test on
//! both arms' counts rejects "no loss". The family is fixed before any data:
//! every (scenario, probe) — a pass rate for a `must_show` probe and a
//! violation rate for a `must_not_show` one —, the pooled `must_show` pass
//! rate, the pooled `must_not_show` violation rate, the failed-run rate and
//! the unjudged-run rate, adjusted with Holm at a family-wise `α = 0.10`. A
//! probe's rate counts judged, non-failed runs only, and a failed run is
//! compared once, as the failed-run rate. The verdict is `fail` when any
//! comparison is a regression, `pass` when none is and every arm has at least
//! 20 judged runs of every scenario, and `inconclusive` otherwise — also when
//! a comparison has no counted run in an arm. There is no absolute pass line.
//!
//! A failed run is one that ended on an error or its wall-clock budget, or
//! one whose judging did not happen: there was nothing the judge could be
//! asked, or a judge call failed — the first call, its retry without a
//! temperature, or a re-ask; a reply that stopped at its output cap fails the
//! call and is not asked again. The judge is asked for `present` or `absent`;
//! the verdict parser it shares with the summary evaluation also accepts
//! `contradicted`, which a `must_not_show` probe treats as not shown. The
//! judge's verdicts are read from a JSON array or from one object per line; a
//! reply that gives no verdict, or leaves a statement without one, is asked
//! again with what was wrong, at most twice.
//! A run the judge answered but still gave no usable verdicts for after the
//! re-asks is unjudged, and so is a run whose view does not fit the judge's
//! budget, which is never asked: either adds nothing to its arm's probe
//! rates, and the report counts it and lists it with its reason. A run that
//! failed on its own is a failed run whatever its judge did: it leaves the
//! probe rates with every other failed run, and the failed-run rate compares
//! it.
//!
//! The judge's window is read from the judge's selection snapshot or from the
//! `--model-config` file, and an evaluation whose judge declares no window
//! fails before its first call. A run's view is judged in one call, and a
//! view that does not fit the window less the reply's `maxTokens` leaves the
//! run unjudged without a call: there is no judging in parts, because a probe
//! that counts, orders or holds over a whole run cannot be answered from one
//! part. On the product profile, until
//! [#522](https://github.com/arcships/dim/issues/522) lands, a model whose
//! limits are not declared carries the catalog's assumed pair (128,000 and
//! 16,384 tokens): only a view over the assumed budget is unjudged with the
//! estimate and the budget, while a view between the real and the assumed
//! budget is sent, refused by the provider, and a failed run. The
//! `--model-config` file declares the window explicitly. A scenario's
//! `budget.window` must equal the run model's declared window, so a model
//! configuration whose run entry is narrowed for the scenarios declares the
//! judge as a second provider entry beside it: the same `baseUrl` and
//! `apiKeyEnv`, its own provider id, and the model's real window and output
//! limit. The catalog keys its models by provider and model id
//! (`crates/dim-agent-aimux/src/models.rs`), so one upstream model can be
//! declared under two providers:
//!
//! ```json
//! {
//!   "defaultModel": {"providerId": "run", "modelId": "model-a"},
//!   "providers": [
//!     {"id": "run", "kind": "openai-chat", "apiKeyEnv": "MODEL_KEY",
//!      "baseUrl": "https://example.invalid/v1",
//!      "models": [{"id": "model-a", "contextWindow": 16000,
//!                  "maxOutputTokens": 1024, "config": {"maxTokens": 1024}}]},
//!     {"id": "judge", "kind": "openai-chat", "apiKeyEnv": "MODEL_KEY",
//!      "baseUrl": "https://example.invalid/v1",
//!      "models": [{"id": "model-a", "contextWindow": 1048576,
//!                  "maxOutputTokens": 8192, "config": {"maxTokens": 8192}}]}
//!   ]
//! }
//! ```
//!
//! The run model above is declared for the scenarios' 16,000-token window and
//! the same upstream model judges with its own 1,048,576-token window, so
//! `--judge judge/model-a` reads a view of that size in one call.
//!
//! Both arms load the product's default plugins, from the default
//! configuration file the product ships (`--defaults FILE`, by default the
//! one in this repository), through the App Server's own default plugin
//! configuration, so the TypeScript plugins run in every scenario. Their
//! rows need Node, the `@arcships/rutis-runtime` packages and each default
//! plugin's own packages: the runner reads `DIM_TEST_NODE` and
//! `DIM_TEST_JS_RUNTIME` from the environment. For example:
//!
//! ```text
//! DIM_TEST_NODE="$(command -v node)" \
//! DIM_TEST_JS_RUNTIME="$PWD/runtime/node_modules/@arcships/rutis-runtime" \
//! cargo run --locked -p xtask -- scenario-eval ...
//! ```
//!
//! A source checkout installs those packages first: `npm ci --prefix
//! runtime --ignore-scripts` for the rutis runtime, and `npm ci --prefix
//! runtime/plugins/<name> --ignore-scripts` for `workflow`, the default
//! plugin that ships a `package.json` (`plugin-authoring` ships only a
//! Skill).
//!
//! A candidate for a TypeScript plugin's text replaces a member of that
//! plugin's factory configuration, which is its manifest `config`: the
//! candidate table names the plugin and the member, and the harness patches
//! the plugin's row, so both arms run the same plugin code and differ only
//! in the configured text.
//!
//! A scenario directory under `--scenarios` holds `scenario.json` and the
//! starting workspace beside it, in `workspace/`; the directory name is the
//! scenario's id:
//!
//! ```json
//! {
//!   "description": "Two independent fixes, one of them on a line",
//!   "message": "Make the failing tests pass.",
//!   "calls": [{"method": "session.goal.set", "params": {"objective": "Keep the suite green."}}],
//!   "sources": [{
//!     "name": "chat",
//!     "guide": "The team cannot see this conversation; answer through chat_reply.",
//!     "types": [{"type": "chat.message", "description": "A chat message.", "mode": "turn"}],
//!     "subscribe": {"types": ["chat.message"]},
//!     "replyTool": "chat_reply"
//!   }],
//!   "events": [{
//!     "source": "chat", "type": "chat.message", "id": "m-1",
//!     "title": "The failing test", "body": "What does parse() do with an empty line?",
//!     "whenToolCall": "read"
//!   }],
//!   "steers": [
//!     {"afterSeconds": 30.0, "text": "Also update the README."},
//!     {"whenToolCall": "config.py", "text": "DEFAULT_TIMEOUT is 45 now."}
//!   ],
//!   "budget": {"turns": 12, "seconds": 600, "window": 131072},
//!   "probes": [
//!     {"id": "one-line-per-fix", "kind": "must_show",
//!      "text": "Two lines, one per independent fix"},
//!     {"id": "no-main-relay", "kind": "must_not_show",
//!      "text": "main relays messages between two lines"}
//!   ]
//! }
//! ```
//!
//! - `calls` is the Session state a scenario needs before its message: a
//!   list of `{method, params}`, sent in order on the scenario's Session
//!   after it is created and before the message. The runner resolves the
//!   Session id into `params.sessionId`; a branch defaults to `main` the
//!   way the methods treat it. A call that fails fails the run, and the
//!   error names the method.
//! - `sources` declares the event producers a scenario runs with and the
//!   Session's subscription to them. One producer row is installed, only
//!   when `sources` is present, and it declares every source through the
//!   `eventHub` service; `types` is that declaration's event types, and
//!   `guide` is the source's guide. The runner subscribes `main` to
//!   `subscribe.types` after `calls` and before the message, with the
//!   declaration done first, so the guide is written as the live
//!   `eventhub:guide:<name>` block from the first turn. Without
//!   `subscribe` the source starts unsubscribed, for a scenario where the
//!   model subscribes itself through the `events` tool. `replyTool` names
//!   an optional tool the row registers for the scenario's Session: it
//!   takes `{messageId, text}`, records the call in the run's record and
//!   answers `sent`. A declaration or a subscription that fails fails the
//!   run, naming the source.
//! - `events` publishes each event through the same service: `source`,
//!   `type`, `id`, `title`, `body`, `attributes` and exactly one trigger:
//!   `afterSeconds` or `whenToolCall`, under the steers' rule, or
//!   `afterSubscribedSeconds`, which publishes it that many seconds after
//!   the first subscription naming its source exists — the model's own
//!   `events` subscribe call, in a scenario whose source starts
//!   unsubscribed (EventHub design A.4). Each receipt
//!   goes in the run's record. A delivery whose status is not `delivered`
//!   fails the run, for every trigger; a receipt with no deliveries fails
//!   the run too, except for an `afterSubscribedSeconds` event, where the
//!   model owns the subscription and missing the event is the run's
//!   outcome. A run does not end while a steer is still to be sent or an
//!   event is still to be published, except an `afterSubscribedSeconds`
//!   event whose clock never started: a model that never subscribed cannot
//!   make it fire, so it is never published and never holds the run open.
//!   A delivery starts a turn unless its mode is `append`. For every
//!   trigger, a delivery's mode is the strongest effective mode among the
//!   subscriptions its delivery lists, each one's own `mode` or else the
//!   type's declared mode, read from `eventhub.subscriptions` just before
//!   the publish. A delivered receipt carries no mode of its own.
//! - `message` is the operator's first message, submitted to `main`. A steer
//!   is one more operator message to `main`, sent when the run reaches
//!   `afterSeconds` of wall clock or, for `whenToolCall`, when a
//!   `step/tool-use` event carries the substring. A steer sets exactly one of
//!   the two. A question the run asks is declined: the scenario scripts no
//!   answer to it. The runner keeps the decline until the call's
//!   `step/tool-result` closes the prompt, and answers a
//!   `question_not_pending` again once a quiet period has passed. That
//!   cadence is the runner's own policy: the client rule has the prompt stay
//!   open until the batch's `step/tool-result` and the user send the answer
//!   again ([Questions §3.2], [Questions §4.3]), and a run has no user, so it
//!   sends again by itself. The message is queued input, as the product's
//!   own first message is; the steers are the operator's and are submitted in
//!   Steer mode, so each one ends a wait and reaches a `main` that is
//!   waiting.
//! - `budget.turns` bounds the turns over every line and `budget.seconds` the
//!   wall clock from the first submission; both stop the run. `budget.window`
//!   is the context window the scenario is written for and is checked, not
//!   set: the window is the model's declared limit, so a run whose Session
//!   declares another one is a failed run. The Session reports it only after
//!   its first request has counted the context, so the check waits for the
//!   run to settle; a run that starts no request has no window to check.
//! - A `must_show` probe passes when the judge finds that the run shows it; a
//!   `must_not_show` probe passes when the run does not, and a run that shows
//!   it is a violation.
//!
//! Every run is isolated (issue #1309): the run's server is served in its
//! own child process with a disposable environment — a fresh `HOME`, `TMPDIR`
//! and `XDG_*` under the run root, `PATH` and the one credential variable the
//! model configuration names, and nothing else of the operator's environment
//! — the execution settings document turns the sandbox on, the file tools
//! carry the root boundary, and a preflight Session checks the run's
//! isolation and its tool set before the model's first turn. A run whose
//! preflight fails, or a host with no sandbox backend, is a harness failure,
//! not a model result.
//!
//! The runner copies the scenario's workspace into a fresh run root for every
//! run — `workspace/` and the disposable environment directories, and nothing
//! else — so runs share no state. The run's Dim home is a fresh directory
//! under `--out` (`homes/`), and its account files (`timeline.jsonl`, the
//! whole Timeline; `judge-view.jsonl`, the view the judge read; the preflight
//! report) live under `--out/outputs/`, never inside the run root. The run's
//! App Server endpoint is a Unix socket under `--out/sockets/`, outside the
//! run root. A run's home, Session, judge calls and endpoint are its own, so
//! `--jobs` needs nothing of them: every run serves its own App Server in its
//! own process, and the runner holds no state a second run reads.
//!
//! A finished evaluation's saved runs are judged again, with no model turn,
//! from the `judge-view.jsonl` each run saved:
//!
//! ```text
//! cargo run --locked -p xtask -- scenario-eval --rejudge DIR --scenarios DIR
//!     --model PROVIDER/MODEL --judge PROVIDER/MODEL --model-config FILE
//!     --out DIR [--jobs N] [--run arm/scenario/run]...
//! ```
//!
//! A re-judged run keeps its own stop, turns and error and takes the judge's
//! part afresh; a run that failed before judging is kept as it is. The source
//! is never written to, and an `--out` inside it is refused before anything
//! is created. Repeated `--run` selects runs, each at most once; without one,
//! every saved run is judged again.
//!
//! Several finished evaluations are reported together under the same rule and
//! format, with no model turn and no judge call, one `--pool` per source — a
//! single one included:
//!
//! ```text
//! cargo run --locked -p xtask -- scenario-eval --pool DIR --pool DIR
//!     --scenarios DIR --model PROVIDER/MODEL --judge PROVIDER/MODEL
//!     --model-config FILE --out DIR
//! ```
//!
//! Each source's `runs.jsonl` is read as it stands, in the order given: the
//! runs are added together and no source is written to. A pooled set keeps
//! the sources' run numbers, which may repeat between them.
//!
//! [Questions §3.2]: ../../docs/design/plugins/Questions.md#32-clients-attach-and-several-clients
//! [Questions §4.3]: ../../docs/design/plugins/Questions.md#43-fork-and-branches
use crate::app_server::{self, Overrides, RunSettings};
use crate::rule::{self, Comparison, Counts, Worse};
use crate::summary_eval::{
    Completion, Configuration, Judgement, Verdict, complete_detailed, parse_judgements,
};
use dim_agent_capabilities::{
    context_model::{output_allowance, window_facts},
    host::{
        HostFuture, ModelHost, ModelSelectionHost, ModelStopReason, ProviderConnectionResolver,
        ResolveContextModel, ToolExecutionContext,
    },
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry, definition},
    workspace_tools::FileToolBoundary,
};
use dim_agent_runtime::{AgentBuilder, Names, PartRegistry, PartScope};
use dim_agent_types::{
    InputMode, Message, ModelConfig, ModelId, ModelRequest, ModelSelection, Role, SystemPrompt,
    ToolCall, ToolDefinition, ToolResult, ToolSource,
};
use dim_app_server::RunningServer;
use dim_client::{Client, ConnectionConfig, Message as WireMessage, Server as WireServer};
use dim_event_hub::{DeclareRequest, EventHub, PublishRequest, TypeDeclaration};
use futures_util::StreamExt;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// How long the driver waits after the last event before it asks whether
/// every line is idle. The quiet period follows the last notice the driver
/// reads, including the notices it reads while it waited on `session.idle`.
const QUIET: Duration = Duration::from_secs(1);
/// How long one `session.idle` call may take before its line counts as busy.
const IDLE_WAIT: Duration = Duration::from_secs(1);
/// How long a failed run waits for its cancelled siblings to settle before
/// their futures are dropped (and their children's groups killed with them).
const SETTLE_BOUND: Duration = Duration::from_secs(30);

const JUDGE_SYSTEM: &str = "You check statements against the exported Timeline of a coding session. The Timeline is JSONL: a header line, one branch line per line of the Session, one event line per event, and an end line. A context/prepare or model/call event carries its request: addedMessages are the messages added since that line's previous request, with messageCount the whole request's count, and messages is the whole list when the history changed; every other field of the request appears in full only when it changed, and otherwise reads {\"unchanged\": true}. For each statement answer \"present\" when the run shows it and \"absent\" when it does not. Add the Timeline events that support the verdict as a short quote. Answer with a JSON array only, one object per statement: {\"id\": \"<statement id>\", \"verdict\": \"present|absent\", \"quote\": \"<Timeline evidence>\"}.";

/// The message that asks the judge again, `{what}` saying why its last answer
/// could not be used.
const JUDGE_REASK: &str = "Your answer could not be used: {what}. Answer again with the JSON array only, one object per statement.";
/// How many times one run's judge is asked again for an answer it cannot use.
const REASKS: u32 = 2;

/// A loopback OpenAI-compatible provider that answers by `reply`, holds an
/// answer by `hold`, and records every request it receives. Every evaluation
/// serves one for the run's preflight session (decision 5 of the isolation
/// work) and the fixture tests script the run and judge answers with it.
pub(crate) struct Provider {
    pub(crate) requests: Mutex<Vec<String>>,
    pub(crate) reply: Reply,
    pub(crate) hold: Hold,
}

/// The provider's answer to one request: the stream event to send, or
/// `None` for a plain text reply.
pub(crate) type Reply = fn(&str) -> Option<Value>;
/// How long the provider holds a request's whole answer back before it
/// sends the first byte of it; `None` sends the answer at once.
pub(crate) type Hold = fn(&str) -> Option<Duration>;
/// The hold of a provider that answers every request at once.
pub(crate) fn holds_nothing(_: &str) -> Option<Duration> {
    None
}

pub(crate) async fn completion(
    axum::extract::State(provider): axum::extract::State<Arc<Provider>>,
    axum::Json(body): axum::Json<Value>,
) -> String {
    let text = body.to_string();
    provider.requests.lock().unwrap().push(text.clone());
    if let Some(hold) = (provider.hold)(&text) {
        tokio::time::sleep(hold).await;
    }
    let event = (provider.reply)(&text).unwrap_or_else(|| content("ok"));
    format!("data: {event}\n\ndata: [DONE]\n\n")
}

/// A text answer in the stream shape the provider speaks.
pub(crate) fn content(text: &str) -> Value {
    json!({"id":"fixture","choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
}

/// A tool call in the stream shape the provider speaks.
pub(crate) fn tool_call(name: &str, arguments: &str) -> Value {
    json!({"id":"fixture","choices":[{"index":0,"delta":{"tool_calls":[{
        "index":0,"id":"call-1","type":"function",
        "function":{"name":name,"arguments":arguments}
    }]},"finish_reason":"tool_calls"}]})
}

/// The `plugin` tool's name: PluginManager's own tool installs a plugin
/// mid-run whose code runs in the serve process, holding the run's
/// credential, constrained by neither the sandbox nor the file-tool
/// boundary. No run's Session offers it.
const PLUGIN_TOOL: &str = "plugin";
/// The tools a run's Sessions declare: the served assembly's whole set —
/// the workspace host's tools and every part's contributed tools — without
/// `plugin`. A Session's mount list fixes what it declares, in this order;
/// without one it declares every host tool and every tool a part registers
/// (config.rs `SessionConfig::tools`, #364). The list stays explicit on
/// purpose: a future tool must not reach a live run unreviewed, as
/// `plugin` shows. A served process registers no `plugin` tool at all
/// (`ServerConfig::plugin_tools` is false there), so the tools it registers
/// — this list's host tools and the run's named plugin defaults'
/// contributions — are the whole set any Session of the process can
/// declare: a Session's tool list selects among registered contributions,
/// whatever created the Session (an `agent` member with an inline or a
/// stored profile, a scheduled launch, a branch). The `tools-product`
/// preflight check compares the probe Session's declared set with this
/// list at run time, so an addition or removal surfaces as a failed check,
/// not a silent drift in what an evaluation offers the model under test;
/// the fixture pin asserts the mounted order of the set.
const RUN_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "glob",
    "grep",
    "exec",
    "exec_stop",
    "agent",
    "ask_user",
    "branch",
    "events",
    "goal_update",
    "member_approval_allow",
    "member_approval_reject",
    "recall",
    "schedule",
    "settings",
    "skill",
    "wait",
    "wakeup",
    "web_fetch",
    "web_search",
];
/// The mount list of a run's Sessions: the product's whole tool set
/// without the `plugin` tool, plus the reply tools the scenario's sources
/// declare (`replyTool`), in declaration order: `ScenarioProducer`'s
/// session part registers exactly those (`ScenarioReplySession`), so a
/// whitelist without them would hide a tool the scenario itself drives.
fn session_tools(scenario: &Scenario) -> Vec<String> {
    let mut tools: Vec<String> = RUN_TOOLS.iter().map(|name| (*name).to_owned()).collect();
    for source in &scenario.sources {
        if let Some(tool) = &source.reply_tool
            && !tools.iter().any(|mounted| mounted == tool)
        {
            tools.push(tool.clone());
        }
    }
    tools
}

/// The offered tool names of a model request body, in declaration order.
fn offered_tools(request: &str) -> Vec<String> {
    serde_json::from_str::<Value>(request)
        .ok()
        .and_then(|body| body["tools"].as_array().cloned())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["function"]["name"].as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The served preflight endpoint of one run: a loopback OpenAI-chat provider
/// whose scripted answers drive the run's preflight Session through every
/// check. Every request after the first is answered as the step whose number
/// of executed tool results the request already holds, so the replies need no
/// state: the requests carry the full conversation, and each executed step's
/// tool result appears in it exactly once, as its `tool_call_id`. (The
/// assistant's tool calls all reuse the id `call-1`, so counting the calls
/// would recount the results; the results' `tool_call_id` key is unique.) The
/// two per-run paths (the run's socket and the
/// operator's home, the probe targets) travel in the preflight message as
/// `SOCKET=<path>` and `OPERATOR_HOME=<path>` lines, which the answers read
/// back from the request text.
/// The preflight message, as the mock reads it from the request: the user
/// message that carries it in the OpenAI-chat body. The request arrives as
/// one compact JSON line, so the message's own newlines are escaped inside
/// it; the mock parses the body rather than splitting the request text, and
/// finds the preflight message by its `SOCKET=` line among the session
/// messages (the first user text in the body is the system prompt). The
/// `OUTPUT_ROOT=` line names the outputs root the account files and the
/// sibling runs' records live under.
fn session_text(text: &str) -> String {
    let text_of = |content: &Value| -> Option<String> {
        match content {
            Value::String(text) => Some(text.clone()),
            Value::Array(parts) => Some(
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        }
    };
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|body| body["messages"].as_array().cloned())
        .and_then(|messages| {
            messages
                .iter()
                .filter_map(|message| text_of(&message["content"]))
                .find(|content| content.contains("SOCKET="))
        })
        .unwrap_or_default()
}

fn preflight_reply(text: &str) -> Option<Value> {
    let session = session_text(text);
    // A product-probe request carries no preflight message; answer it
    // without a tool call, so the probe turns never execute anything and
    // the scripted steps stay keyed to the preflight conversation.
    if !session.contains("SOCKET=") {
        return Some(content("PRODUCT TOOLS DONE"));
    }
    let path = |token: &str| {
        session
            .lines()
            .find_map(|line| line.strip_prefix(token))
            .unwrap_or_default()
            .to_owned()
    };
    match text.matches("\"tool_call_id\"").count() {
        0 => Some(tool_call("exec", r#"{"command":"echo $HOME"}"#)),
        1 => Some(tool_call("exec", r#"{"command":"env"}"#)),
        2 => Some(tool_call(
            "exec",
            r#"{"command":"cat ../preflight-outside"}"#,
        )),
        3 => Some(tool_call("read", r#"{"path":"../preflight-outside"}"#)),
        4 => Some(tool_call(
            "exec",
            &format!(r#"{{"command":"ls {}"}}"#, path("SOCKET=")),
        )),
        5 => Some(tool_call(
            "exec",
            &format!(r#"{{"command":"ls {}"}}"#, path("OPERATOR_HOME=")),
        )),
        6 => Some(tool_call(
            "read",
            &format!(r#"{{"path":"{}"}}"#, path("OPERATOR_HOME=")),
        )),
        7 => Some(tool_call(
            "exec",
            &format!(
                r#"{{"command":"cat {}/preflight-outside"}}"#,
                path("OUTPUT_ROOT=")
            ),
        )),
        8 => Some(tool_call(
            "read",
            &format!(r#"{{"path":"{}/preflight-outside"}}"#, path("OUTPUT_ROOT=")),
        )),
        _ => Some(content("PREFLIGHT DONE")),
    }
}

/// Where one text lives in the assembly [`app_server::serve_home`] builds.
enum TextMember {
    /// A Rust part's factory configuration member, put in place by this
    /// function.
    Rust(fn(&mut Overrides, Arc<str>)),
    /// A TypeScript plugin's factory configuration member: the plugin's
    /// name in the default configuration file, and the dotted path of the
    /// member inside the manifest's `config`. The harness patches that
    /// plugin's row, so both arms run the same plugin code (model-prompts
    /// §8.1).
    ///
    /// The `eval-tests` text-injector fixture is its only constructor: no
    /// shipped text is a TypeScript plugin member any more. The attribute is
    /// unconditional because the fixture is a test target: with the feature
    /// on, the bin target still never constructs the variant.
    #[allow(dead_code)]
    Plugin {
        plugin: &'static str,
        member: &'static str,
    },
}
/// The policy texts a candidate can replace, and the factory configuration
/// member each is put in place of in the assembly [`app_server::serve_home`]
/// builds. The summary prompt (ContextCompaction §13.5) and the branch tool's
/// texts (timeline branches §12.4) are the texts exposed at a Rust factory
/// configuration today; the other owners add their entries with their own
/// factory members: Capabilities under
/// [#551](https://github.com/arcships/dim/issues/551), and ContextCompaction
/// under [#554](https://github.com/arcships/dim/issues/554).
/// A plugin's text is its manifest `config` member, which the plugin's row
/// carries into its `apply`.
const TEXTS: &[(&str, TextMember)] = &[
    (
        "context-compaction/summary",
        TextMember::Rust(|overrides, text| overrides.summary_prompt = Some(text)),
    ),
    (
        "timeline-tree/branch-description",
        TextMember::Rust(|overrides, text| overrides.branch_description = Some(text)),
    ),
    (
        "timeline-tree/roster-closing",
        TextMember::Rust(|overrides, text| overrides.roster_closing = Some(text)),
    ),
    (
        "goal/objective",
        TextMember::Rust(|overrides, text| overrides.goal_objective = Some(text)),
    ),
    (
        "goal/continuation",
        TextMember::Rust(|overrides, text| overrides.goal_continuation = Some(text)),
    ),
    (
        "eventhub/entry",
        TextMember::Rust(|overrides, text| overrides.eventhub_entry = Some(text)),
    ),
    (
        "eventhub/tool",
        TextMember::Rust(|overrides, text| overrides.eventhub_tool = Some(text)),
    ),
    (
        "scheduler/description",
        TextMember::Rust(|overrides, text| overrides.scheduler_description = Some(text)),
    ),
];

#[cfg(unix)]
pub async fn run() -> Result<()> {
    let options = Options::parse()?;
    let report = match (&options.rejudge, options.pool.is_empty()) {
        (Some(source), _) => rejudge(&options, source, |_| {}).await?,
        (None, false) => pooled(&options, |_| {}).await?,
        (None, true) => evaluate(&options).await?,
    };
    println!("{report}");
    println!("wrote {}", options.out.join("scenario.md").display());
    Ok(())
}

/// A scenario evaluation cannot isolate a run off unix: the run's own App
/// Server endpoint, the sandbox backend and the file-tool boundary are
/// unix-only, so the entry refuses at startup with the reason instead of
/// failing per run.
#[cfg(not(unix))]
pub async fn run() -> Result<()> {
    Err(
        "scenario-eval runs only on unix: a run's App Server endpoint, the sandbox \
         and the file-tool boundary are unix-only"
            .into(),
    )
}

/// Runs the evaluation and writes `runs.jsonl` and `scenario.md`; returns the
/// report.
async fn evaluate(options: &Options) -> Result<String> {
    if options.defaults.is_some() {
        if options.node.is_none() {
            return Err("set DIM_TEST_NODE: the default plugins run in Node".into());
        }
        if options.js_runtime.is_none() {
            return Err("set DIM_TEST_JS_RUNTIME: the default plugins import it".into());
        }
    }
    let candidate = overrides(&options.texts)?;
    let current = Overrides::default();
    let scenarios = read_scenarios(&options.scenarios)?;
    // A live evaluation is isolated: the run's server must come from a
    // model configuration, which also names the credential its calls use.
    if options.model_config.is_none() {
        return Err("a live evaluation needs --model-config: the run's server is served in its own process from the model configuration it names (isolate each live scenario run)".into());
    }
    let cancel = CancellationToken::new();
    // The differential native probe runs once per evaluation, before the
    // first run: one child served exactly as a run child is served but
    // without the plugin defaults, so its probe Session declares the
    // assembly's native tool set. Every run's tools-product check reads its
    // declared set against it.
    let native = native_probe(options, &cancel).await?;
    let instrument = Instrument::new(options);
    // Every run keeps its own instrument, merged in schedule order below, so
    // nothing in the report depends on which run finished first.
    let schedule = schedule(options.runs, scenarios.len());
    let mut pending: Vec<Option<(RunRecord, Instrument)>> =
        (0..schedule.len()).map(|_| None).collect();
    let (scenarios, cancel) = (&scenarios, &cancel);
    let planned = schedule
        .iter()
        .enumerate()
        .map(|(index, &(arm, scenario, run))| {
            let mut measured = instrument.clone();
            let overrides = match arm {
                "current" => &current,
                _ => &candidate,
            };
            let native = &native;
            async move {
                let record = run_once(
                    options,
                    arm,
                    overrides,
                    &scenarios[scenario],
                    run,
                    cancel,
                    native,
                    &mut measured,
                )
                .await?;
                Ok::<_, Box<dyn std::error::Error>>((index, record, measured))
            }
        });
    let mut in_flight = futures_util::stream::iter(planned).buffer_unordered(options.jobs);
    while let Some(finished) = in_flight.next().await {
        match finished {
            Ok((index, record, measured)) => pending[index] = Some((record, measured)),
            Err(error) => {
                // A failed run cancels its in-flight siblings before the
                // harness returns: their children drive the model and write
                // under --out, so they must settle first. The bound keeps a
                // stuck sibling from holding the harness; a sibling still
                // unsettled when it expires is dropped, and its child's
                // process group killed with it.
                cancel.cancel();
                let settle = async { while in_flight.next().await.is_some() {} };
                let _ = tokio::time::timeout(SETTLE_BOUND, settle).await;
                return Err(error);
            }
        }
    }
    drop(in_flight);
    let mut instrument = instrument;
    let mut records = Vec::with_capacity(schedule.len());
    for entry in pending {
        let (record, measured) = entry.ok_or("a scheduled run finished without a record")?;
        instrument.merge(&record, measured);
        records.push(record);
    }
    let mut jsonl = String::new();
    for record in &records {
        jsonl += &serde_json::to_string(record)?;
        jsonl.push('\n');
    }
    std::fs::write(options.out.join("runs.jsonl"), jsonl)?;
    let report = report(&instrument, scenarios, &records);
    std::fs::write(options.out.join("scenario.md"), &report)?;
    Ok(report)
}

/// `--rejudge`: judges a finished evaluation's saved runs again from their
/// `judge-view.jsonl`, without a model turn, and writes a fresh `runs.jsonl`
/// and report to `--out`. A run that failed before judging is kept as it is
/// and is not judged again; every other run keeps its own outcome — `stop`,
/// `turns`, `error` — and gains the judge's part afresh. The source is never
/// written to.
async fn rejudge(
    options: &Options,
    source: &Path,
    observe: impl Fn(&mut AgentBuilder),
) -> Result<String> {
    let source = std::fs::canonicalize(source)
        .map_err(|error| format!("--rejudge {}: {error}", source.display()))?;
    let saved = read_saved_runs(&source)?;
    let selected = select_saved(&saved, &options.select)?;
    let scenarios = read_scenarios(&options.scenarios)?;
    let mut plan = Vec::with_capacity(selected.len());
    for run in selected {
        let index = scenarios
            .iter()
            .position(|scenario| scenario.id == run.scenario)
            .ok_or_else(|| {
                format!(
                    "--rejudge: no scenario {} under {}",
                    run.scenario,
                    options.scenarios.display()
                )
            })?;
        plan.push((run, index));
    }
    let cancel = CancellationToken::new();
    let server = served(options, observe).await?;
    let outcome = rejudge_with(options, &source, &server, &scenarios, &plan, &cancel).await;
    server.shutdown();
    server.join().await?;
    let (records, model, judge, limits) = outcome?;
    let sources = Sources {
        directories: vec![(source.clone(), records.len())],
        given: false,
    };
    write_report(options, &scenarios, &records, model, judge, limits, sources)
}

/// `--pool`: reports over several finished evaluations' saved runs under the
/// same rule and report format, without a model turn or a judge call. Each
/// source's `runs.jsonl` is read as it stands, in the order given; the
/// sources are never written to, and nothing is re-judged. A pooled set
/// keeps the sources' run numbers, which may repeat between them.
async fn pooled(options: &Options, observe: impl Fn(&mut AgentBuilder)) -> Result<String> {
    let scenarios = read_scenarios(&options.scenarios)?;
    let mut records = Vec::new();
    let mut directories = Vec::new();
    for source in &options.pool {
        let source = std::fs::canonicalize(source)
            .map_err(|error| format!("--pool {}: {error}", source.display()))?;
        let read = read_pooled_runs(&source, &scenarios)?;
        directories.push((source, read.len()));
        records.extend(read);
    }
    let cancel = CancellationToken::new();
    let server = served(options, observe).await?;
    let resolved = async {
        let model = selection(&server, &options.model, &options.out, &cancel).await?;
        let judge = selection(&server, &options.judge, &options.out, &cancel).await?;
        let limits = judge_window(&judge, options.model_config.as_deref(), &cancel).await?;
        Ok::<_, Box<dyn std::error::Error>>((model, judge, limits))
    }
    .await;
    server.shutdown();
    server.join().await?;
    let (model, judge, limits) = resolved?;
    let sources = Sources {
        directories,
        given: true,
    };
    write_report(options, &scenarios, &records, model, judge, limits, sources)
}

/// The profile a re-judge or a pool resolves the judge in: the model
/// configuration under a fresh home beside the output. No Session is created
/// and no model turn runs.
async fn served(options: &Options, observe: impl Fn(&mut AgentBuilder)) -> Result<RunningServer> {
    let home = options.out.join("home");
    private_directory(&home)?;
    app_server::serve_home(
        &home,
        &options.out,
        options.out.join("command-output"),
        RunSettings {
            model_config: options.model_config.clone(),
            ..Default::default()
        },
        observe,
    )
    .await
}

/// Writes a set of runs as `--out`'s `runs.jsonl` and the report over them as
/// `scenario.md`, under the rule of model-prompts §8.1, and returns the
/// report. `sources` names the finished evaluations the runs were read from.
fn write_report(
    options: &Options,
    scenarios: &[Scenario],
    records: &[RunRecord],
    model: ModelSelection,
    judge: ModelSelection,
    limits: JudgeWindow,
    sources: Sources,
) -> Result<String> {
    let mut jsonl = String::new();
    for record in records {
        jsonl += &serde_json::to_string(record)?;
        jsonl.push('\n');
    }
    std::fs::write(options.out.join("runs.jsonl"), jsonl)?;
    let mut instrument = Instrument {
        texts: options.texts.iter().map(|(text, _)| text.clone()).collect(),
        model: options.model.clone(),
        judge: options.judge.clone(),
        runs: runs_per_arm_and_scenario(records),
        tokens: options
            .texts
            .iter()
            .map(|(text, candidate)| (text.clone(), text_tokens(&options.model, candidate)))
            .collect(),
        model_configuration: Some(Configuration::read(model.config.as_ref())),
        judge_configuration: Some(Configuration::read(judge.config.as_ref())),
        judge_window: Some(limits),
        sources: Some(sources),
        judge_temperature_dropped: 0,
        judge_temperature_unrecorded: 0,
        judge_asked_runs: 0,
        judge_reasks: 0,
        decline_retries: 0,
        decline_retries_unrecorded: 0,
    };
    let measured = instrument.clone();
    for record in records {
        instrument.merge(record, measured.clone());
    }
    let report = report(&instrument, scenarios, records);
    std::fs::write(options.out.join("scenario.md"), &report)?;
    Ok(report)
}

/// One `--pool` source's saved runs, matched to the scenarios the report
/// names.
fn read_pooled_runs(source: &Path, scenarios: &[Scenario]) -> Result<Vec<RunRecord>> {
    read_saved_runs(source)?
        .into_iter()
        .map(|run| pooled_record(source, run, scenarios))
        .collect()
}

/// One pooled run as the report reads it: each probe keeps the kind its
/// source recorded, checked against the scenario's, so a probe that changed
/// kind since is refused rather than reinterpreted, and a record naming a
/// scenario or a probe the set does not have is refused too.
fn pooled_record(source: &Path, run: SavedRun, scenarios: &[Scenario]) -> Result<RunRecord> {
    let path = source.join("runs.jsonl");
    let scenario = scenarios
        .iter()
        .find(|scenario| scenario.id == run.scenario)
        .ok_or_else(|| {
            format!(
                "{}: no scenario {} under the scenarios directory",
                path.display(),
                run.scenario
            )
        })?;
    let probes = run
        .probes
        .into_iter()
        .map(|probe| {
            let recorded = probe.kind.ok_or_else(|| {
                format!(
                    "{}: scenario {} probe {} recorded no kind; re-run the evaluation",
                    path.display(),
                    scenario.id,
                    probe.id
                )
            })?;
            let declared = scenario
                .probes
                .iter()
                .find(|declared| declared.id == probe.id)
                .ok_or_else(|| {
                    format!(
                        "{}: no probe {} in scenario {}",
                        path.display(),
                        probe.id,
                        scenario.id
                    )
                })?;
            let kind = declared.kind.name();
            if recorded != kind {
                return Err(format!(
                    "{}: scenario {} probe {} is {kind} now but its run recorded {recorded}; \
                     re-run the evaluation",
                    path.display(),
                    scenario.id,
                    probe.id
                )
                .into());
            }
            Ok(ProbeRecord {
                id: probe.id,
                kind,
                verdict: probe.verdict,
                quote: probe.quote,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RunRecord {
        arm: arm_name(&run.arm)?,
        scenario: run.scenario,
        run: run.run,
        session: run.session,
        stop: stop_name(&run.stop)?,
        turns: run.turns,
        window: run.window,
        duration_ms: run.duration_ms,
        failed: run.failed,
        judge_outcome: run.judge_outcome,
        error: run.error,
        judge_error: run.judge_error,
        judge_reasks: run.judge_reasks,
        judge_calls: run.judge_calls,
        temperature_dropped: run.temperature_dropped,
        decline_retries: run.decline_retries,
        events: run.events,
        reply_calls: run.reply_calls,
        probes,
    })
}

/// The judge part of a re-judge: resolves the judge and the run model from the
/// served profile, judges the selected runs at `--jobs` at once, and returns
/// the records in plan order with the selections it resolved.
async fn rejudge_with(
    options: &Options,
    source: &Path,
    server: &RunningServer,
    scenarios: &[Scenario],
    plan: &[(&SavedRun, usize)],
    cancel: &CancellationToken,
) -> Result<(Vec<RunRecord>, ModelSelection, ModelSelection, JudgeWindow)> {
    let (Some(host), Some(resolver)) = (
        server.service::<ModelHost>(),
        server.service::<ProviderConnectionResolver>(),
    ) else {
        return Err("the served profile has no model host for the judge".into());
    };
    let judge = selection(server, &options.judge, &options.out, cancel).await?;
    let model = selection(server, &options.model, &options.out, cancel).await?;
    let limits = judge_window(&judge, options.model_config.as_deref(), cancel).await?;
    let (source, host, resolver, cancel) = (source, &host, &resolver, cancel);
    let judge = &judge;
    let mut pending: Vec<Option<RunRecord>> = (0..plan.len()).map(|_| None).collect();
    let planned = plan.iter().enumerate().map(|(index, (run, scenario))| {
        let scenario = &scenarios[*scenario];
        async move {
            let record =
                rejudge_one(source, host, resolver, judge, scenario, run, limits, cancel).await?;
            Ok::<_, Box<dyn std::error::Error>>((index, record))
        }
    });
    let mut in_flight = futures_util::stream::iter(planned).buffer_unordered(options.jobs);
    while let Some(finished) = in_flight.next().await {
        let (index, record) = finished?;
        pending[index] = Some(record);
    }
    drop(in_flight);
    let mut records = Vec::with_capacity(plan.len());
    for entry in pending {
        records.push(entry.ok_or("a scheduled re-judge finished without a record")?);
    }
    Ok((records, model, judge.clone(), limits))
}

/// One saved run judged again: a run that failed before judging keeps its
/// outcome and is not judged; every other run's view is read from its saved
/// `judge-view.jsonl` and judged afresh.
#[allow(clippy::too_many_arguments)]
async fn rejudge_one(
    source: &Path,
    host: &Arc<ModelHost>,
    resolver: &Arc<ProviderConnectionResolver>,
    judge: &ModelSelection,
    scenario: &Scenario,
    saved: &SavedRun,
    limits: JudgeWindow,
    cancel: &CancellationToken,
) -> Result<RunRecord> {
    let judging = if saved.judge_outcome == JudgeOutcome::NotAsked {
        Judging::not_asked(scenario, saved.judge_error.clone())
    } else {
        let path = source
            .join("outputs")
            .join(&saved.arm)
            .join(&saved.scenario)
            .join(format!("run-{}.judge-view.jsonl", saved.run));
        let view = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        judge_export(host, resolver, judge, scenario, &view, limits, cancel).await
    };
    let stop = stop_name(&saved.stop)?;
    Ok(RunRecord {
        arm: arm_name(&saved.arm)?,
        scenario: saved.scenario.clone(),
        run: saved.run,
        session: saved.session.clone(),
        stop,
        turns: saved.turns,
        window: saved.window,
        duration_ms: saved.duration_ms,
        failed: failed(stop, saved.error.as_deref(), judging.outcome),
        judge_outcome: judging.outcome,
        error: saved.error.clone(),
        judge_error: judging.error,
        judge_reasks: judging.reasks,
        judge_calls: judging.calls,
        temperature_dropped: Some(judging.temperature_dropped),
        decline_retries: saved.decline_retries,
        events: saved.events.clone(),
        reply_calls: saved.reply_calls.clone(),
        probes: judging.probes,
    })
}

/// One run of a finished evaluation's `runs.jsonl`, read back where a
/// re-judge or a pool takes it in: a re-judge keeps the run's own outcome and
/// judges it afresh; a pool reports the record as it stands.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedRun {
    arm: String,
    scenario: String,
    run: usize,
    #[serde(default)]
    session: Option<String>,
    stop: String,
    #[serde(default)]
    turns: u32,
    #[serde(default)]
    window: Option<f64>,
    #[serde(default)]
    duration_ms: f64,
    #[serde(default)]
    failed: bool,
    judge_outcome: JudgeOutcome,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    judge_error: Option<String>,
    #[serde(default)]
    judge_reasks: u32,
    #[serde(default)]
    judge_calls: Vec<JudgeCall>,
    /// `None` in a record written before the runner wrote this fact.
    #[serde(default)]
    temperature_dropped: Option<bool>,
    /// How many declines the runner sent again; `None` in a record written
    /// before the runner recorded it.
    #[serde(default)]
    decline_retries: Option<u32>,
    /// The events the run published, as the run recorded them; `None` in a
    /// record written before the runner published any.
    #[serde(default)]
    events: Vec<EventRecord>,
    /// The reply-tool calls the run made; empty in a record written before
    /// the runner recorded them.
    #[serde(default)]
    reply_calls: Vec<ReplyCall>,
    #[serde(default)]
    probes: Vec<SavedProbe>,
}

/// One probe's record in a saved run: the kind the run was judged against,
/// its verdict and the judge's quote.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedProbe {
    id: String,
    /// The kind the run was judged against; every record this tool writes
    /// carries it, so one without it is refused rather than guessed at.
    #[serde(default)]
    kind: Option<String>,
    verdict: ProbeVerdict,
    #[serde(default)]
    quote: Option<String>,
}

/// The runs a finished evaluation wrote, in their recorded order.
fn read_saved_runs(source: &Path) -> Result<Vec<SavedRun>> {
    let path = source.join("runs.jsonl");
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut runs = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        runs.push(
            serde_json::from_str(line)
                .map_err(|error| format!("{}:{}: {error}", path.display(), index + 1))?,
        );
    }
    Ok(runs)
}

/// The saved runs `--run` selects, in the order given; no selection means
/// every run the source holds.
fn select_saved<'a>(saved: &'a [SavedRun], select: &[String]) -> Result<Vec<&'a SavedRun>> {
    if select.is_empty() {
        return Ok(saved.iter().collect());
    }
    select
        .iter()
        .map(|value| {
            let (arm, scenario, run) = parse_selection(value)?;
            saved
                .iter()
                .find(|saved| saved.arm == arm && saved.scenario == scenario && saved.run == run)
                .ok_or_else(|| format!("--run {value}: the source has no such run").into())
        })
        .collect()
}

/// `arm/scenario/run`, the shape `--run` takes: the run as its index or as
/// its directory name `run-N`.
fn parse_selection(value: &str) -> Result<(&str, &str, usize)> {
    let mut parts = value.split('/');
    let (Some(arm), Some(scenario), Some(run), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("--run {value}: a run is arm/scenario/run").into());
    };
    if arm != "current" && arm != "candidate" {
        return Err(format!("--run {value}: an arm is current or candidate").into());
    }
    let index = run.strip_prefix("run-").unwrap_or(run);
    let run = index
        .parse::<usize>()
        .map_err(|_| format!("--run {value}: {run} is not a run index"))?;
    Ok((arm, scenario, run))
}

/// A saved record's arm as the runner's static name.
fn arm_name(arm: &str) -> Result<&'static str> {
    match arm {
        "current" => Ok("current"),
        "candidate" => Ok("candidate"),
        other => Err(format!("the saved run has an unknown arm {other}").into()),
    }
}

/// A saved record's stop as the runner's static name.
fn stop_name(stop: &str) -> Result<&'static str> {
    match stop {
        "idle" => Ok("idle"),
        "turns" => Ok("turns"),
        "seconds" => Ok("seconds"),
        "error" => Ok("error"),
        other => Err(format!("the saved run has an unknown stop {other}").into()),
    }
}

/// The most runs any one arm and scenario has in this set: the count the
/// report's header names.
fn runs_per_arm_and_scenario(records: &[RunRecord]) -> usize {
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for record in records {
        *counts
            .entry((record.arm, record.scenario.as_str()))
            .or_default() += 1;
    }
    counts.values().copied().max().unwrap_or(0)
}

/// The evaluation's runs in the order it schedules them: for each run index,
/// every scenario, `current` then `candidate` (model-prompts §8.1). The
/// order is fixed before any run starts, so the records are deterministic
/// whatever order the runs complete in, and `--jobs` changes only how many
/// runs are in flight.
fn schedule(runs: usize, scenarios: usize) -> Vec<(&'static str, usize, usize)> {
    let mut schedule = Vec::with_capacity(runs * scenarios * 2);
    for run in 0..runs {
        for scenario in 0..scenarios {
            for arm in ["current", "candidate"] {
                schedule.push((arm, scenario, run));
            }
        }
    }
    schedule
}

struct Options {
    scenarios: PathBuf,
    /// The `--text ID --candidate FILE` pairs, in the order given.
    texts: Vec<(String, Arc<str>)>,
    model: ModelId,
    judge: ModelId,
    runs: usize,
    /// How many runs are in flight at once; the schedule does not depend on
    /// it.
    jobs: usize,
    out: PathBuf,
    model_config: Option<PathBuf>,
    /// Whether each run is isolated: its server runs in its own process
    /// with a disposable environment, the sandbox is on, the file tools
    /// carry the root boundary, and the preflight is enforced. The command
    /// line always isolates; the offline tests turn it off for an ablation.
    isolate: bool,
    /// A test hook that faults the preflight phase, to exercise the abort.
    fail_preflight: bool,
    /// A test hook that offers the `plugin` tool to the run's Sessions,
    /// to exercise the tools check.
    offer_plugin: bool,
    /// A test hook that contributes one tool `RUN_TOOLS` does not name from
    /// a part the run's plugin defaults do not name, to exercise the
    /// tools-product check.
    extra_tool: bool,
    /// A test hook that makes one arm's run child wait for a marker file —
    /// written once the other arm's model call is held — and then exit
    /// non-zero, to exercise a failed run cancelling a still-working
    /// sibling: the arm's name and the marker's path.
    fail_child: Option<(String, PathBuf)>,
    /// A test hook beside `fail_child`: the hook's descendant inherits the
    /// child's standard output instead of detaching it, so only killing the
    /// child's group lets the parent's read reach EOF.
    fail_child_holds_stdout: bool,
    /// The default plugin configuration file both arms load; `None` serves
    /// the assembly without the product's plugins.
    defaults: Option<PathBuf>,
    node: Option<PathBuf>,
    js_runtime: Option<PathBuf>,
    /// The finished evaluation `--rejudge` reads: its saved runs are judged
    /// again from their views, and no model turn runs.
    rejudge: Option<PathBuf>,
    /// The runs `--rejudge` reads, as `arm/scenario/run`; empty means every
    /// run the source holds.
    select: Vec<String>,
    /// The finished evaluations `--pool` reports over, in the order given:
    /// their saved runs are added as they stand, and no model turn runs.
    pool: Vec<PathBuf>,
}
impl Options {
    fn parse() -> Result<Self> {
        Self::parse_from(std::env::args().skip(2))
    }
    /// [`parse`](Self::parse) over the arguments after the command name:
    /// `--out` is created and resolved here, so every path a run builds from
    /// it — the workspace, `command-output`, `runs.jsonl` and the report — is
    /// absolute, as `session.create` requires of the cwd.
    fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let usage = "usage: cargo run -p xtask -- scenario-eval --scenarios DIR \
                     --text ID --candidate FILE [--text ID --candidate FILE]... \
                     --model PROVIDER/MODEL --judge PROVIDER/MODEL --runs N \
                     --out DIR [--jobs N] [--model-config FILE] \
                     [--defaults FILE]; a text named twice is an error, and a \
                     run whose view does not fit the judge's own declared \
                     window is unjudged, so a model configuration narrowed for \
                     the scenarios declares the judge under its own provider \
                     entry (see the module doc). --rejudge DIR judges a finished \
                     evaluation's saved views again without a model turn, with \
                     --scenarios, --judge, --model, --model-config, --out and \
                     [--jobs N], selecting runs with repeated --run \
                     arm/scenario/run; --pool DIR adds a finished evaluation's \
                     saved runs to the report and may be repeated, writing the \
                     same rule over their union";
        // Both arms serve the product's default plugins; `--defaults`
        // names another default configuration file.
        let mut args = args.into_iter();
        let (mut scenarios, mut out) = (None, None);
        let (mut text, mut candidate, mut model, mut judge) = (None, None, None, None);
        let (mut runs, mut jobs, mut model_config, mut defaults) = (None, None, None, None);
        let mut rejudge = None;
        let mut select: Vec<String> = Vec::new();
        let mut pool: Vec<PathBuf> = Vec::new();
        let mut texts: Vec<(String, Arc<str>)> = Vec::new();
        while let Some(flag) = args.next() {
            let value = args.next().ok_or(format!("{flag} requires a value"))?;
            match flag.as_str() {
                "--scenarios" => scenarios = Some(PathBuf::from(value)),
                "--rejudge" => rejudge = Some(PathBuf::from(value)),
                "--run" => {
                    let run = parse_selection(&value)?;
                    if select
                        .iter()
                        .any(|given| parse_selection(given).is_ok_and(|given| given == run))
                    {
                        return Err(format!("--run {value} names a run already selected").into());
                    }
                    select.push(value);
                }
                "--pool" => pool.push(PathBuf::from(value)),
                "--text" => {
                    if let Some(pending) = &text {
                        return Err(format!("--text {pending} has no --candidate").into());
                    }
                    if texts.iter().any(|(given, _)| *given == value) {
                        return Err(format!("--text {value} is named twice").into());
                    }
                    match candidate.take() {
                        Some(prompt) => texts.push((value, prompt)),
                        None => text = Some(value),
                    }
                }
                "--candidate" => {
                    let prompt = candidate_prompt(Path::new(&value))?;
                    match text.take() {
                        Some(id) => texts.push((id, prompt)),
                        None if candidate.is_none() => candidate = Some(prompt),
                        None => return Err("--candidate has no --text".into()),
                    }
                }
                "--model" => model = Some(target(&value)?),
                "--judge" => judge = Some(target(&value)?),
                "--runs" => runs = Some(value.parse::<usize>()?),
                "--jobs" => jobs = Some(value.parse::<usize>()?),

                "--out" => out = Some(PathBuf::from(value)),
                "--model-config" => model_config = Some(PathBuf::from(value)),
                "--defaults" => defaults = Some(PathBuf::from(value)),
                _ => return Err(usage.into()),
            }
        }
        if let Some(text) = &text {
            return Err(format!("--text {text} has no --candidate").into());
        }
        if candidate.is_some() {
            return Err("--candidate has no --text".into());
        }
        if texts.is_empty() && rejudge.is_none() && pool.is_empty() {
            return Err(usage.into());
        }
        if rejudge.is_some() && !pool.is_empty() {
            return Err("--rejudge and --pool are different reads: give one".into());
        }
        if !pool.is_empty() && !select.is_empty() {
            return Err(
                "--run selects a --rejudge source's runs; --pool reports every saved run".into(),
            );
        }
        if !select.is_empty() && rejudge.is_none() {
            return Err("--run selects a --rejudge source's runs: give --rejudge DIR".into());
        }
        let finished = rejudge.is_some() || !pool.is_empty();
        if finished && model_config.is_none() {
            return Err(
                "--rejudge and --pool need --model-config: the judge's window is read from it"
                    .into(),
            );
        }
        if finished && (!texts.is_empty() || runs.is_some() || defaults.is_some()) {
            return Err(
                "--rejudge and --pool read finished evaluations' saved runs: --text, \
                 --candidate, --runs and --defaults belong to a new evaluation"
                    .into(),
            );
        }
        let defaults = Some(defaults.unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime/default-plugins.json")
        }));
        // A finished evaluation's runs come from its sources, not from --runs.
        let runs = if finished {
            0
        } else {
            runs.filter(|runs| *runs > 0).ok_or(usage)?
        };
        let out = out.ok_or(usage)?;
        let scenarios = scenarios.ok_or(usage)?;
        let model = model.ok_or(usage)?;
        let judge = judge.ok_or(usage)?;
        let resolved = resolve_output(&out)?;
        refuse_output_inside_sources(&out, &resolved, rejudge.as_deref(), &pool)?;
        std::fs::create_dir_all(&resolved)
            .map_err(|error| format!("--out {}: {error}", out.display()))?;
        Ok(Self {
            scenarios,
            texts,
            model,
            judge,
            runs,
            jobs: jobs.unwrap_or(1).max(1),
            out: resolved,
            model_config,
            defaults,
            node: env_path("DIM_TEST_NODE"),
            js_runtime: env_path("DIM_TEST_JS_RUNTIME"),
            isolate: true,
            fail_preflight: false,
            offer_plugin: false,
            extra_tool: false,
            fail_child: None,
            fail_child_holds_stdout: false,
            rejudge,
            select,
            pool,
        })
    }
}

/// A path from the environment: the TypeScript plugin rows run in Node,
/// which the runner reads from `DIM_TEST_NODE` and `DIM_TEST_JS_RUNTIME`.
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Resolves `--out` once, before anything is created, to the path every run
/// writes under: `session.create` takes it as an absolute cwd.
///
/// A component that exists is resolved with `canonicalize`, so a link is
/// followed as the OS follows it and `..` is resolved against the real
/// parent. After the first component that does not exist, the rest are the
/// directories `create_dir_all` will create as real directories: `.` is
/// skipped, a name is appended and `..` removes the last appended name,
/// going back to the filesystem once the missing part is emptied. A link
/// whose target does not exist is refused: the resolution cannot say where
/// creating through it would land. The result has no `.` or `..` and no link
/// in its existing part, in the form `canonicalize` gives a source, so
/// `starts_with` compares like with like.
fn resolve_output(out: &Path) -> Result<PathBuf> {
    let absolute = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("--out {}: {error}", out.display()))?
            .join(out)
    };
    let root = absolute
        .ancestors()
        .last()
        .expect("an absolute path has a root");
    let mut resolved =
        std::fs::canonicalize(root).map_err(|error| format!("--out {}: {error}", out.display()))?;
    let mut missing: Vec<&OsStr> = Vec::new();
    for component in absolute
        .strip_prefix(root)
        .expect("the root is a prefix of the path")
        .components()
    {
        match component {
            Component::CurDir => {}
            Component::ParentDir if !missing.is_empty() => {
                missing.pop();
            }
            Component::ParentDir => {
                resolved = std::fs::canonicalize(resolved.join(".."))
                    .map_err(|error| format!("--out {}: {error}", out.display()))?;
            }
            Component::Normal(name) if !missing.is_empty() => missing.push(name),
            Component::Normal(name) => {
                let path = resolved.join(name);
                match std::fs::canonicalize(&path) {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let link = std::fs::symlink_metadata(&path)
                            .is_ok_and(|metadata| metadata.file_type().is_symlink());
                        if link {
                            return Err(format!(
                                "--out {}: {} is a link to a path that does not exist",
                                out.display(),
                                path.display()
                            )
                            .into());
                        }
                        missing.push(name);
                    }
                    Err(error) => return Err(format!("--out {}: {error}", out.display()).into()),
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                unreachable!("the root of an absolute path is resolved first")
            }
        }
    }
    for name in missing {
        resolved.push(name);
    }
    Ok(resolved)
}

/// A finished read never writes to its sources: refuses an `--out` inside
/// one. It compares the path `--out` resolved to, the path that is then
/// created, so the path checked is the path created and a refusal leaves the
/// source as it was. The message names `--out` as given.
fn refuse_output_inside_sources(
    out: &Path,
    resolved: &Path,
    rejudge: Option<&Path>,
    pool: &[PathBuf],
) -> Result<()> {
    if rejudge.is_none() && pool.is_empty() {
        return Ok(());
    }
    for (flag, source) in rejudge
        .into_iter()
        .map(|source| ("--rejudge", source))
        .chain(pool.iter().map(|source| ("--pool", source.as_path())))
    {
        let source = std::fs::canonicalize(source)
            .map_err(|error| format!("{flag} {}: {error}", source.display()))?;
        if resolved.starts_with(&source) {
            return Err(format!(
                "--out {} is inside {flag} {}: the source is never written to",
                out.display(),
                source.display()
            )
            .into());
        }
    }
    Ok(())
}

fn candidate_prompt(path: &Path) -> Result<Arc<str>> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("--candidate {}: {error}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("--candidate {}: not UTF-8 text", path.display()))?;
    Ok(Arc::from(text))
}

fn target(value: &str) -> Result<ModelId> {
    let (provider_id, model_id) = value
        .split_once('/')
        .ok_or_else(|| format!("a model is PROVIDER/MODEL, not {value}"))?;
    Ok(ModelId {
        provider_id: provider_id.into(),
        model_id: model_id.into(),
    })
}

/// The overrides that put every candidate in place of the text its id names in
/// [`TEXTS`]; an unknown id fails before any run and lists the known ids.
fn overrides(texts: &[(String, Arc<str>)]) -> Result<Overrides> {
    overrides_in(TEXTS, texts)
}

/// [`overrides`] over `table`: a test reaches a member no shipped text
/// exposes yet with its own entry, through the same injection.
fn overrides_in(table: &[(&str, TextMember)], texts: &[(String, Arc<str>)]) -> Result<Overrides> {
    let mut overrides = Overrides::default();
    for (text, candidate) in texts {
        let Some((_, member)) = table.iter().find(|(id, _)| id == text) else {
            let known: Vec<_> = table.iter().map(|(id, _)| *id).collect();
            return Err(format!(
                "--text {text}: unknown text id; known ids: {}",
                known.join(", ")
            )
            .into());
        };
        match member {
            TextMember::Rust(apply) => apply(&mut overrides, candidate.clone()),
            TextMember::Plugin { plugin, member } => {
                let value = Value::String(candidate.to_string());
                set_member(&mut overrides.plugin_configs, plugin, member, value);
            }
        }
    }
    Ok(overrides)
}

/// Puts `value` at the dotted `member` of a plugin's factory configuration,
/// keeping the members an earlier pair set: two texts of one plugin are two
/// members of one configuration, not two configurations.
fn set_member(configs: &mut BTreeMap<String, Value>, plugin: &str, member: &str, value: Value) {
    let mut path: Vec<&str> = member.split('.').collect();
    let leaf = path.pop().expect("a member has a path");
    let object = || Value::Object(serde_json::Map::new());
    let mut target = configs.entry(plugin.to_owned()).or_insert_with(object);
    for key in path {
        if !target.is_object() {
            *target = object();
        }
        let map = target.as_object_mut().expect("an object was put in place");
        target = map.entry(key.to_owned()).or_insert_with(object);
    }
    if !target.is_object() {
        *target = object();
    }
    target
        .as_object_mut()
        .expect("an object was put in place")
        .insert(leaf.to_owned(), value);
}

/// The candidate text's own token cost, counted with the product's counter
/// (`window_facts().count_tokens`) on the request the text travels in. The
/// count reads the request's bytes; the window it is built with does not
/// enter it.
pub(crate) fn text_tokens(model: &ModelId, text: &str) -> f64 {
    let mut request = ModelRequest::new(&model.provider_id, &model.model_id, vec![]);
    request.system = Some(Arc::new(SystemPrompt::Text(text.into())));
    window_facts(0, 0., &request).count_tokens(&request)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScenarioFile {
    #[serde(default)]
    description: String,
    message: String,
    /// Session method calls made before the message, in order.
    #[serde(default)]
    calls: Vec<Call>,
    /// The EventHub sources the runner declares and subscribes the Session
    /// to before the message.
    #[serde(default)]
    sources: Vec<Source>,
    /// The events the runner publishes while the run is under way.
    #[serde(default)]
    events: Vec<EventSpec>,
    #[serde(default)]
    steers: Vec<Steer>,
    budget: Budget,
    probes: Vec<Probe>,
}
/// One source a scenario declares through the `eventHub` service, and
/// optionally subscribes its Session's `main` branch to (EventHub design A.1,
/// A.3). Without `subscribe` the source starts unsubscribed, for a scenario
/// where the model subscribes through the `events` tool. `Serialize` carries
/// the scenario to the serve child.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Source {
    pub(crate) name: String,
    /// The source's manual for the model, as the declaration carries it.
    pub(crate) guide: String,
    /// The declaration's event types.
    pub(crate) types: Vec<SourceType>,
    #[serde(default)]
    pub(crate) subscribe: Option<Subscribed>,
    /// The tool name the runner registers for the Session, when the guide
    /// names one; the tool takes `{messageId, text}` and answers `sent`.
    #[serde(default)]
    pub(crate) reply_tool: Option<String>,
}
/// One entry of a source's declared types (EventHub design A.1).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SourceType {
    pub(crate) r#type: String,
    #[serde(default)]
    pub(crate) description: String,
    pub(crate) mode: InputMode,
}
/// The subscription a scenario's source asks for (EventHub design A.3).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Subscribed {
    pub(crate) types: Vec<String>,
    #[serde(default)]
    pub(crate) mode: Option<InputMode>,
}
/// One event the runner publishes, and when: the steers' trigger rule.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EventSpec {
    pub(crate) source: String,
    pub(crate) r#type: String,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) attributes: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) after_seconds: Option<f64>,
    #[serde(default)]
    pub(crate) when_tool_call: Option<String>,
    /// Publish this many seconds after the first subscription naming the
    /// event's source exists, so a scenario where the model subscribes
    /// through the `events` tool can time the event (EventHub design A.4).
    #[serde(default)]
    pub(crate) after_subscribed_seconds: Option<f64>,
    /// When the runner first saw a subscription naming the event's source:
    /// the `afterSubscribedSeconds` clock, set by the runner, not the file.
    #[serde(skip)]
    pub(crate) subscribed_at: Option<Instant>,
    /// The event type's declared mode: the effective mode of a subscription
    /// that sets none (EventHub design §3.1).
    #[serde(skip)]
    pub(crate) declared_mode: InputMode,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Call {
    pub(crate) method: String,
    /// The method's parameters without the Session id, which the runner
    /// resolves.
    #[serde(default)]
    pub(crate) params: Value,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Steer {
    #[serde(default)]
    pub(crate) after_seconds: Option<f64>,
    #[serde(default)]
    pub(crate) when_tool_call: Option<String>,
    pub(crate) text: String,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Budget {
    pub(crate) turns: u32,
    pub(crate) seconds: f64,
    pub(crate) window: u64,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Probe {
    id: String,
    kind: ProbeKind,
    text: String,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProbeKind {
    MustShow,
    MustNotShow,
}
impl ProbeKind {
    fn name(self) -> &'static str {
        match self {
            Self::MustShow => "must_show",
            Self::MustNotShow => "must_not_show",
        }
    }
}

/// A scenario as the runner runs it: its directory (its workspace is
/// `workspace/` beside the file), the script and the probes.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Scenario {
    pub(crate) id: String,
    pub(crate) description: String,
    /// The scenario's directory; the serve child never uses it, and a child
    /// deserialization leaves it empty.
    #[serde(default)]
    pub(crate) directory: PathBuf,
    pub(crate) message: String,
    pub(crate) calls: Vec<Call>,
    pub(crate) sources: Vec<Source>,
    pub(crate) events: Vec<EventSpec>,
    pub(crate) steers: Vec<Steer>,
    pub(crate) budget: Budget,
    probes: Vec<Probe>,
}

pub(crate) fn read_scenarios(directory: &Path) -> Result<Vec<Scenario>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .map_err(|error| format!("--scenarios {}: {error}", directory.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    entries.sort();
    if entries.is_empty() {
        return Err(format!(
            "--scenarios {}: no scenario directories",
            directory.display()
        )
        .into());
    }
    let mut scenarios = Vec::new();
    for entry in entries {
        let id = entry
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{}: the directory name is not UTF-8", entry.display()))?
            .to_owned();
        let file = entry.join("scenario.json");
        let bytes = std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
        let raw: ScenarioFile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", file.display()))?;
        let scenario = into_scenario(id, entry, raw)
            .map_err(|error| format!("{}: {error}", file.display()))?;
        scenarios.push(scenario);
    }
    Ok(scenarios)
}

fn into_scenario(
    id: String,
    directory: PathBuf,
    mut raw: ScenarioFile,
) -> std::result::Result<Scenario, String> {
    if raw.budget.turns == 0 {
        return Err("budget.turns must be positive".into());
    }
    if !raw.budget.seconds.is_finite() || raw.budget.seconds <= 0. {
        return Err("budget.seconds must be positive".into());
    }
    if raw.budget.window == 0 {
        return Err("budget.window must be positive".into());
    }
    for steer in &raw.steers {
        match (&steer.after_seconds, &steer.when_tool_call) {
            (Some(after), None) if *after > 0. => {}
            (None, Some(trigger)) if !trigger.is_empty() => {}
            _ => {
                return Err(format!(
                    "a steer sets exactly one of afterSeconds (positive) and whenToolCall (non-empty): {}",
                    steer.text
                ));
            }
        }
    }
    for call in &raw.calls {
        if call.method.trim().is_empty() {
            return Err("a call needs a method".into());
        }
        if !call.params.is_object() && !call.params.is_null() {
            return Err(format!("call {}: params must be an object", call.method));
        }
    }
    if raw.sources.is_empty() && !raw.events.is_empty() {
        return Err("an event needs the source it comes from declared in sources".into());
    }
    let mut names = BTreeSet::new();
    let mut tools = BTreeSet::new();
    for source in &raw.sources {
        if source.name.trim().is_empty() {
            return Err("a source needs a name".into());
        }
        if !names.insert(source.name.clone()) {
            return Err(format!("source {}: the name is used twice", source.name));
        }
        if source.types.is_empty() {
            return Err(format!("source {}: declare at least one type", source.name));
        }
        if let Some(subscribe) = &source.subscribe
            && subscribe.types.is_empty()
        {
            return Err(format!(
                "source {}: subscribe to at least one type",
                source.name
            ));
        }
        if let Some(tool) = &source.reply_tool {
            if tool.trim().is_empty() {
                return Err(format!(
                    "source {}: the reply tool needs a name",
                    source.name
                ));
            }
            if !tools.insert(tool.clone()) {
                return Err(format!(
                    "source {}: the reply tool {tool} is used twice",
                    source.name
                ));
            }
        }
    }
    for event in &mut raw.events {
        match (
            &event.after_seconds,
            &event.when_tool_call,
            &event.after_subscribed_seconds,
        ) {
            (Some(after), None, None) if *after > 0. => {}
            (None, Some(trigger), None) if !trigger.is_empty() => {}
            (None, None, Some(after)) if *after > 0. => {}
            _ => {
                return Err(format!(
                    "an event sets exactly one of afterSeconds (positive), whenToolCall (non-empty) and afterSubscribedSeconds (positive): {}",
                    event.id
                ));
            }
        }
        if event.id.trim().is_empty() {
            return Err("an event needs an id".into());
        }
        let Some(source) = raw
            .sources
            .iter()
            .find(|source| source.name == event.source)
        else {
            return Err(format!(
                "event {}: source {} is not declared in sources",
                event.id, event.source
            ));
        };
        let Some(declared) = source
            .types
            .iter()
            .find(|declared| declared.r#type == event.r#type)
        else {
            return Err(format!(
                "event {}: type {} is not declared by source {}",
                event.id, event.r#type, event.source
            ));
        };
        // The type's declared mode applies to a delivery whose subscription
        // sets none; a subscription's own mode wins, from the snapshot the
        // publish's receipt is read against (EventHub design §3.1).
        event.declared_mode = declared.mode;
    }
    if raw.probes.is_empty() {
        return Err("a scenario needs at least one probe".into());
    }
    let mut ids = BTreeSet::new();
    for probe in &raw.probes {
        if probe.id.trim().is_empty() {
            return Err("a probe needs an id".into());
        }
        if probe.text.trim().is_empty() {
            return Err(format!("probe {}: the statement is empty", probe.id));
        }
        if !ids.insert(probe.id.clone()) {
            return Err(format!("probe {}: the id is used twice", probe.id));
        }
    }
    if !directory.join("workspace").is_dir() {
        return Err("the scenario needs a workspace/ directory".into());
    }
    Ok(Scenario {
        id,
        description: raw.description,
        directory,
        message: raw.message,
        calls: raw.calls,
        sources: raw.sources,
        events: raw.events,
        steers: raw.steers,
        budget: raw.budget,
        probes: raw.probes,
    })
}

/// One run of one arm on one scenario.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RunRecord {
    arm: &'static str,
    scenario: String,
    run: usize,
    session: Option<String>,
    /// `idle`, `turns`, `seconds` or `error`.
    stop: &'static str,
    turns: u32,
    window: Option<f64>,
    duration_ms: f64,
    /// Whether the run is a failed run, set once when the record is built
    /// (model-prompts §8.1).
    failed: bool,
    /// How the run's judging ended (model-prompts §8.1).
    judge_outcome: JudgeOutcome,
    error: Option<String>,
    judge_error: Option<String>,
    /// How many times the judge was asked again for an answer it could not
    /// use.
    judge_reasks: u32,
    /// One entry per judge call the run made, in order: what its stream
    /// reported.
    judge_calls: Vec<JudgeCall>,
    /// Whether the judge had to drop its temperature; the runner records it
    /// for every run it makes, and `None` when a report reads a record from
    /// before this fact was written.
    temperature_dropped: Option<bool>,
    /// How many declines the runner sent again after `question_not_pending`;
    /// `None` in a record written before the runner recorded it.
    decline_retries: Option<u32>,
    /// One entry per event the run published, in order: the receipt EventHub
    /// answered (EventHub design A.2). A receipt that is not `delivered`
    /// fails the run and is recorded here as it came.
    events: Vec<EventRecord>,
    /// One entry per reply-tool call the run's tools recorded, in order.
    reply_calls: Vec<ReplyCall>,
    probes: Vec<ProbeRecord>,
}
impl RunRecord {
    /// Whether the run is unjudged (model-prompts §8.1): the judge was not
    /// asked because the view of the run's export does not fit its budget, or
    /// it answered without usable verdicts after the re-asks, and nothing
    /// else failed the run. Such a run enters none of its arm's probe rates;
    /// the unjudged-run rate compares it.
    fn unjudged(&self) -> bool {
        !self.failed
            && matches!(
                self.judge_outcome,
                JudgeOutcome::Unreadable | JudgeOutcome::Unfit
            )
    }
    /// Whether the run is judged (model-prompts §8.1): it failed neither on
    /// its own nor on its judging, and the judge answered every call with
    /// usable verdicts. Only such a run counts toward the floor.
    fn judged(&self) -> bool {
        !self.failed && self.judge_outcome == JudgeOutcome::Judged
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeRecord {
    id: String,
    kind: &'static str,
    verdict: ProbeVerdict,
    quote: Option<String>,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProbeVerdict {
    Present,
    Absent,
    Contradicted,
    Unparsed,
}
/// How one run's judging ended (model-prompts §8.1). The outcome is a typed
/// field, so nothing tells a run the judge could not be asked from one it
/// answered unreadably by reading the error's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum JudgeOutcome {
    /// The judge answered every call, and its verdicts are usable.
    Judged,
    /// A judge call failed: a window overflow, a transport or an HTTP error,
    /// or a reply cut off at its output cap, on the first call, on its retry,
    /// or on a re-ask.
    CallFailed,
    /// The judge was never asked: the run left no export, the profile has no
    /// model host for the judge, or the run failed before the judge ran.
    NotAsked,
    /// The judge was never asked: the view of the run's export does not fit
    /// the judge's budget. The run is unjudged.
    Unfit,
    /// The judge answered every call and gave no usable verdicts after the
    /// re-asks.
    Unreadable,
}

/// One judge call, as the stream reported it: the stop reason, how long it
/// took, the reply's text, and the reasoning it carried. The OpenAI-chat
/// stream gives no separate reasoning-token count, so the reasoning is the
/// bytes of thinking deltas, and the stream's usage adds the call's whole
/// completion count beside it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JudgeCall {
    /// The stop reason the stream reported: `end_turn`, `tool_use`,
    /// `stop_sequence` or `max_tokens`; `none` when the call failed.
    stop: String,
    duration_ms: f64,
    /// The reply text's length in bytes.
    text_bytes: usize,
    /// The completion tokens the stream reported, when it reported usage.
    completion_tokens: Option<f64>,
    /// The bytes of reasoning deltas the stream carried.
    thinking_bytes: usize,
}
impl JudgeCall {
    /// The call a reply answers, timed from the call's start.
    fn answered(reply: &Completion, duration: Duration) -> Self {
        Self {
            stop: match reply.stop {
                ModelStopReason::EndTurn => "end_turn",
                ModelStopReason::ToolUse => "tool_use",
                ModelStopReason::StopSequence => "stop_sequence",
                ModelStopReason::MaxTokens => "max_tokens",
            }
            .to_owned(),
            duration_ms: duration.as_secs_f64() * 1000.,
            text_bytes: reply.text.len(),
            completion_tokens: reply.completion_tokens,
            thinking_bytes: reply.thinking_bytes,
        }
    }
    /// The call that failed: no stop, no text, and no reply facts.
    fn failed(duration: Duration) -> Self {
        Self {
            stop: "none".to_owned(),
            duration_ms: duration.as_secs_f64() * 1000.,
            text_bytes: 0,
            completion_tokens: None,
            thinking_bytes: 0,
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// One run's serve child: the App Server of the run, served in its own
/// process with a disposable environment (isolation decision 1). The parent
/// starts its own binary with `scenario-eval-serve <params>`; the offline
/// tests start the same entry through the `eval_server_child` test function,
/// so live and offline runs go through the same child-process path.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChildParams {
    /// The run's Dim home, a fresh directory under the output root, never
    /// the operator's `--home`.
    home: PathBuf,
    /// The run's workspace, the session cwd.
    workspace: PathBuf,
    /// The disposable `HOME` of the child, under the run root outside the
    /// workspace.
    env_home: PathBuf,
    /// The run's output root (`--out/command-output`).
    output_root: PathBuf,
    /// The run's endpoint, under the output root outside the run root.
    socket: PathBuf,
    /// The run's model configuration; the child adds its own loopback
    /// preflight provider to it before serving.
    model_config: PathBuf,
    /// The default plugin configuration, when the assembly serves it.
    defaults: Option<PathBuf>,
    node: Option<PathBuf>,
    js_runtime: Option<PathBuf>,
    /// The assembly overrides the arm runs with, as JSON.
    overrides: Value,
    /// The scenario as JSON.
    scenario: Value,
    model: ModelId,
    judge: ModelId,
    /// The file-tool root boundary of the run: workspace, disposable home
    /// and the platform's read-only roots.
    boundary: Option<FileToolBoundary>,
    /// The outputs root of the run's arm: where the judge views and the
    /// sibling runs' records live, the probe target of the output-root
    /// checks.
    outputs: PathBuf,
    /// The environment names the parent passed through for the model
    /// configuration's providers.
    credentials: Vec<String>,
    /// The operator's home, the probe target of the operator-home checks;
    /// `None` skips them.
    operator_home: Option<PathBuf>,
    /// Whether the run is isolated: the execution settings document with
    /// the sandbox on, the file-tool boundary, and the enforced preflight.
    /// The offline tests turn it off for an ablation.
    isolate: bool,
    /// A test hook that fails the preflight phase before it starts, to
    /// exercise the abort.
    fail_preflight: bool,
    /// A test hook that offers the `plugin` tool to the run's Sessions,
    /// to exercise the tools check.
    offer_plugin: bool,
    /// A test hook that contributes one tool `RUN_TOOLS` does not name from
    /// a part the run's plugin defaults do not name, to exercise the
    /// tools-product check.
    #[serde(default)]
    extra_tool: bool,
    /// A test hook: this child waits for the marker file — written once
    /// the sibling arm's model call is held — bounded, then exits non-zero,
    /// so a failed run cancels a sibling still working.
    #[serde(default)]
    fail_child: Option<PathBuf>,
    /// A test hook beside `fail_child`: the hook's descendant inherits this
    /// child's standard output instead of detaching it, so only killing the
    /// child's group lets the parent's read reach EOF.
    #[serde(default)]
    fail_child_holds_stdout: bool,
    /// The native probe's declared set: the assembly served without the
    /// plugin defaults. Each run's `tools-product` check reads its own
    /// declared set against it.
    #[serde(default)]
    native_tools: Vec<(String, ToolSource)>,
    /// This child is the evaluation's differential native probe: it serves
    /// the assembly without the plugin defaults and prints the probe
    /// Session's declared tool set instead of driving a run.
    #[serde(default)]
    probe_only: bool,
}

/// One preflight check's outcome.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreflightCheck {
    pub(crate) name: String,
    pub(crate) passed: bool,
    pub(crate) detail: String,
}

/// The run's serve child prints one JSON line on standard output: the run's
/// record pieces, the export and the view, the configurations the report
/// reads, and the preflight checks. A `harness` error aborts the run before
/// (or instead of) it: the evaluation reports it as a harness failure, not a
/// model result.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChildOutcome {
    preflight: Vec<PreflightCheck>,
    /// The harness failure, if the run was aborted by the harness: no
    /// sandbox backend, a failed preflight, a serve or connect failure.
    harness: Option<String>,
    export: Option<String>,
    view: Option<String>,
    session: Option<String>,
    stop: String,
    turns: u32,
    window: Option<f64>,
    duration_ms: f64,
    decline_retries: u32,
    error: Option<String>,
    judge_outcome: JudgeOutcome,
    judge_error: Option<String>,
    judge_reasks: u32,
    judge_calls: Vec<JudgeCall>,
    temperature_dropped: bool,
    /// The events the run published, as the run's producer recorded them
    /// (EventHub design A.2), and the reply-tool calls the run's tools made.
    #[serde(default)]
    events: Vec<EventRecord>,
    #[serde(default)]
    reply_calls: Vec<ReplyCall>,
    probes: Vec<SavedProbe>,
    /// The reasoning effort and service tier the run's requests carried.
    model_configuration: Option<(Option<String>, Option<String>)>,
    judge_configuration: Option<(Option<String>, Option<String>)>,
    judge_window: (u64, u64),
}

/// The child outcome of a run the harness aborted.
fn harness_outcome(error: String, preflight: Vec<PreflightCheck>) -> ChildOutcome {
    ChildOutcome {
        preflight,
        harness: Some(error),
        export: None,
        view: None,
        session: None,
        stop: "error".into(),
        turns: 0,
        window: None,
        duration_ms: 0.,
        decline_retries: 0,
        error: None,
        judge_outcome: JudgeOutcome::NotAsked,
        judge_error: None,
        judge_reasks: 0,
        judge_calls: Vec::new(),
        temperature_dropped: false,
        events: Vec::new(),
        reply_calls: Vec::new(),
        probes: Vec::new(),
        model_configuration: None,
        judge_configuration: None,
        judge_window: (0, 0),
    }
}

/// The hidden `xtask scenario-eval-serve` entry: reads one run's params file
/// and runs the child.
#[cfg(unix)]
pub(crate) async fn serve_entry(params_path: Option<String>) -> Result<()> {
    let path = params_path.ok_or("scenario-eval-serve needs a params file path")?;
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("{}: {error}", Path::new(&path).display()))?;
    let params: ChildParams = serde_json::from_str(&text)
        .map_err(|error| format!("{}: {error}", Path::new(&path).display()))?;
    // The run's own preflight provider answers in the child, so its model
    // requests — what the Session declared — are the child's to check.
    let (preflight_url, provider, _) = serve_preflight().await?;
    // The run's model configuration with that provider added, beside the
    // run's params.
    let config_path = Path::new(&path).with_extension("model.json");
    augment_model_config(&params.model_config, &preflight_url, &config_path)?;
    let mut params = params;
    params.model_config = config_path;
    // A test hook: the test writes `marker` once the sibling arm's model
    // call is held; this child waits for that event, bounded, then exits
    // non-zero. The wait's outcome is printed on a line of its own, so the
    // parent's error says which happened instead of leaving a missed event
    // to the exit status.
    if let Some(marker) = &params.fail_child {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let seen = loop {
            if marker.exists() {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let outcome = if seen {
            "the held sibling's model call was seen"
        } else {
            "the held sibling's model call was not seen within the bound"
        };
        // The descendant a failed child leaves behind when the harness does
        // not kill its group: `sleep` joins this child's process group (it
        // starts no session of its own), and its pid goes beside the marker
        // for the test to check. Its streams are detached, or — when the
        // test asks for it — it inherits this child's standard output, so
        // only killing the group lets the parent's read reach EOF.
        let mut descendant = std::process::Command::new("sleep");
        descendant
            .arg("300")
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        if params.fail_child_holds_stdout {
            descendant.stdout(Stdio::inherit());
        } else {
            descendant.stdout(Stdio::null());
        }
        let descendant = descendant.spawn().map_err(|error| {
            format!("the failing-child test hook cannot start its descendant: {error}")
        })?;
        std::fs::write(
            marker.with_extension("descendant"),
            descendant.id().to_string(),
        )
        .map_err(|error| {
            format!("the failing-child test hook cannot record its descendant: {error}")
        })?;
        println!("{}", json!({"failChild": outcome}));
        return Err(format!("the failing-child test hook: {outcome}").into());
    }
    if params.probe_only {
        // The differential native probe: the assembly served without the
        // plugin defaults, its probe Session's declared set printed for the
        // parent's runs to check against.
        let declared = native_probe_child(&params).await?;
        println!(
            "{}",
            serde_json::to_string(&json!({"nativeTools": declared}))?
        );
        return Ok(());
    }
    println!(
        "{}",
        serde_json::to_string(&child_outcome(&params, &provider).await)?
    );
    Ok(())
}

/// A non-unix host never starts a serve child: the entry refuses with the
/// same reason [`run`] does.
#[cfg(not(unix))]
pub(crate) async fn serve_entry(_: Option<String>) -> Result<()> {
    Err("scenario-eval-serve runs only on unix".into())
}

/// The child: serves the run's App Server, drives its preflight and its
/// Session over the run's socket, exports the Timeline and judges it, all in
/// its own process with its own environment. `preflight` is the run's own
/// loopback provider, whose request list the tools check reads.
async fn child_outcome(params: &ChildParams, preflight: &Arc<Provider>) -> ChildOutcome {
    let scenario: Scenario = match serde_json::from_value(params.scenario.clone()) {
        Ok(scenario) => scenario,
        Err(error) => {
            return harness_outcome(
                format!("the scenario does not deserialize: {error}"),
                Vec::new(),
            );
        }
    };
    let unparsed = unparsed(&scenario);
    let settings = RunSettings {
        overrides: child_overrides(&params.overrides),
        model_config: Some(params.model_config.clone()),
        unix_path: Some(params.socket.clone()),
        defaults: params.defaults.clone(),
        node: params.node.clone(),
        js_runtime: params.js_runtime.clone(),
        boundary: params.boundary.clone(),
        plugin_tools: false,
    };
    // The scenario's event producers run in the child, whose process
    // publishes what the run's Session subscribes to (EventHub design A.1).
    let producer = Arc::new(ProducerState::default());
    let server = match app_server::serve_home(
        &params.home,
        &params.workspace,
        params.output_root.clone(),
        settings.clone(),
        |builder| {
            if !scenario.sources.is_empty() {
                install_producer(builder, &scenario.sources, producer.clone());
            }
            if params.extra_tool {
                install_extra_tool(builder);
            }
        },
    )
    .await
    {
        Ok(server) => server,
        Err(error) => {
            return harness_outcome(
                format!("the run server failed to start: {error}"),
                Vec::new(),
            );
        }
    };
    let started = Instant::now();
    let outcome = child_drive(
        &server, &settings, params, &scenario, &unparsed, &producer, preflight,
    )
    .await;
    let duration_ms = started.elapsed().as_secs_f64() * 1000.;
    server.shutdown();
    let _ = server.join().await;
    let mut outcome = outcome.unwrap_or_else(|error| {
        harness_outcome(format!("the run failed to finish: {error}"), Vec::new())
    });
    if outcome.duration_ms == 0. {
        outcome.duration_ms = duration_ms;
    }
    outcome
}

/// The native probe child: the assembly served as a run child is served —
/// the same home, workspace and boundary — but without the plugin defaults,
/// and one probe Session whose declared set the child prints for the
/// parent's runs. The probe's one turn is answered by the child's own
/// loopback provider and calls no tool.
async fn native_probe_child(params: &ChildParams) -> Result<Vec<(String, ToolSource)>> {
    let settings = RunSettings {
        overrides: child_overrides(&params.overrides),
        model_config: Some(params.model_config.clone()),
        unix_path: Some(params.socket.clone()),
        defaults: None,
        node: None,
        js_runtime: None,
        boundary: params.boundary.clone(),
        plugin_tools: false,
    };
    let server = app_server::serve_home(
        &params.home,
        &params.workspace,
        params.output_root.clone(),
        settings.clone(),
        |builder| {
            if params.extra_tool {
                install_extra_tool(builder);
            }
        },
    )
    .await
    .map_err(|error| format!("the native probe server failed to start: {error}"))?;
    let connected: std::result::Result<_, String> = async {
        let executable = std::env::current_exe()
            .map_err(|error| format!("the child has no own executable: {error}"))?;
        let mut client = Client::connect(&ConnectionConfig {
            endpoint: params.socket.clone(),
            server: WireServer {
                program: executable,
                embedded: true,
            },
            profile: None,
            client_kind: "other",
            client_name: "scenario-eval",
            protocol_version: 4,
        })
        .await
        .map_err(|error| format!("cannot connect to the served App Server: {error}"))?;
        product_probe(&mut client, params, &settings.database(&params.home)).await
    }
    .await;
    server.shutdown();
    let _ = server.join().await;
    Ok(connected?)
}

/// The drive half of the child, after the run's server is up.
async fn child_drive(
    server: &RunningServer,
    settings: &RunSettings,
    params: &ChildParams,
    scenario: &Scenario,
    unparsed: &[ProbeRecord],
    producer: &ProducerState,
    preflight: &Arc<Provider>,
) -> std::result::Result<ChildOutcome, Box<dyn std::error::Error>> {
    let cancel = CancellationToken::new();
    let socket = settings
        .unix_path
        .clone()
        .ok_or("a run serves its App Server on a local endpoint")?;
    let mut client = Client::connect(&ConnectionConfig {
        endpoint: socket,
        server: WireServer {
            program: std::env::current_exe()?,
            embedded: true,
        },
        profile: None,
        client_kind: "other",
        client_name: "scenario-eval",
        protocol_version: 4,
    })
    .await
    .map_err(|error| format!("cannot connect to the served App Server: {error}"))?;
    let checks = Vec::new();
    if params.isolate {
        let backend = client
            .rpc("host.execution.backend", json!({}))
            .await
            .map_err(|error| error.to_string())?;
        if !backend["available"].as_bool().unwrap_or(false) {
            let reason = backend["reason"].as_str().unwrap_or("no backend");
            return Ok(harness_outcome(
                format!("no sandbox backend, so a live run cannot isolate its commands: {reason}"),
                checks,
            ));
        }
        let revision = client
            .rpc(
                "settings.read",
                json!({"source": "execution", "scope": "user"}),
            )
            .await
            .map_err(|error| error.to_string())?["revision"]
            .clone();
        let _ = client
            .rpc(
                "settings.update",
                json!({
                    "source": "execution", "scope": "user",
                    "expectedRevision": revision,
                    "document": {"sandbox": true, "network": false},
                }),
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    if settings.defaults.is_some() {
        wait_for_plugins(&mut client).await?;
    }
    let model = selection(server, &params.model, &params.workspace, &cancel)
        .await
        .map_err(|error| error.to_string())?;
    let judge = selection(server, &params.judge, &params.workspace, &cancel)
        .await
        .map_err(|error| error.to_string())?;
    let limits = judge_window(&judge, Some(&params.model_config), &cancel)
        .await
        .map_err(|error| error.to_string())?;
    // The preflight runs in every mode — its checks observe the run's
    // actual behavior. An isolated run aborts on a failed check; an
    // unisolated run (an offline ablation) reports the checks as they are
    // and runs on.
    let mut ceiling = None;
    let (checks, preflight) = if params.isolate && params.fail_preflight {
        (
            vec![PreflightCheck {
                name: "hook".into(),
                passed: false,
                detail: "the preflight phase is faulted by the test hook".into(),
            }],
            Some("the preflight phase is faulted by the test hook".to_owned()),
        )
    } else {
        let expected = session_tools(scenario);
        match preflight_session(
            &mut client,
            params,
            preflight,
            &expected,
            &settings.database(&params.home),
        )
        .await
        {
            Ok((checks, tools)) => {
                ceiling = Some(tools);
                (checks, None)
            }
            Err((checks, error)) => {
                let error = if params.isolate { Some(error) } else { None };
                (checks, error)
            }
        }
    };
    let finished = match preflight {
        None => match drive_once(
            server,
            settings,
            &params.home,
            &params.workspace,
            scenario,
            &model,
            &judge,
            limits,
            &cancel,
            producer,
            (!params.offer_plugin)
                .then(|| ceiling.clone().unwrap_or_else(|| session_tools(scenario))),
        )
        .await
        {
            Ok(finished) => finished,
            Err(error) => Finished::failed(scenario, error.to_string()),
        },
        Some(error) => {
            return Ok(harness_outcome(format!("preflight: {error}"), checks));
        }
    };
    let driven = finished.driven;
    let judging = finished.judging;
    Ok(ChildOutcome {
        preflight: checks,
        harness: None,
        export: finished.export,
        view: finished.view,
        session: (!driven.session.is_empty()).then_some(driven.session),
        stop: driven.stop.to_owned(),
        turns: driven.turns,
        window: driven.window,
        duration_ms: 0.,
        decline_retries: driven.decline_retries,
        error: driven.error,
        judge_outcome: judging.outcome,
        judge_error: judging.error,
        judge_reasks: judging.reasks,
        judge_calls: judging.calls,
        temperature_dropped: judging.temperature_dropped,
        events: producer.events(),
        reply_calls: producer.reply_calls(),
        probes: if judging.probes.is_empty() {
            unparsed
                .iter()
                .map(|probe| SavedProbe {
                    id: probe.id.clone(),
                    kind: Some(probe.kind.to_owned()),
                    verdict: probe.verdict,
                    quote: probe.quote.clone(),
                })
                .collect()
        } else {
            judging
                .probes
                .iter()
                .map(|probe| SavedProbe {
                    id: probe.id.clone(),
                    kind: Some(probe.kind.to_owned()),
                    verdict: probe.verdict,
                    quote: probe.quote.clone(),
                })
                .collect()
        },
        model_configuration: carried_parts(&instrument_configuration(&model)),
        judge_configuration: carried_parts(&instrument_configuration(&judge)),
        judge_window: (limits.window, limits.max_output),
    })
}

/// The configuration a selection's request carries, as the report reads it.
fn instrument_configuration(selection: &ModelSelection) -> Configuration {
    Configuration::read(selection.config.as_ref())
}

fn carried_parts(configuration: &Configuration) -> Option<(Option<String>, Option<String>)> {
    Some((configuration.effort.clone(), configuration.tier.clone()))
}

/// The assembly overrides as JSON, and back.
///
/// Every field is destructured first so a member added to [`Overrides`] but
/// missing from this round trip fails the build, as `child_overrides`' struct
/// literal already does.
fn overrides_value(overrides: &Overrides) -> Value {
    let Overrides {
        summary_prompt,
        plugin_configs,
        branch_description,
        roster_closing,
        goal_objective,
        goal_continuation,
        eventhub_entry,
        eventhub_tool,
        scheduler_description,
    } = overrides;
    json!({
        "summaryPrompt": summary_prompt.as_ref().map(ToString::to_string),
        "eventhubEntry": eventhub_entry.as_ref().map(ToString::to_string),
        "eventhubTool": eventhub_tool.as_ref().map(ToString::to_string),
        "pluginConfigs": plugin_configs,
        "branchDescription": branch_description.as_ref().map(ToString::to_string),
        "rosterClosing": roster_closing.as_ref().map(ToString::to_string),
        "goalObjective": goal_objective.as_ref().map(ToString::to_string),
        "goalContinuation": goal_continuation.as_ref().map(ToString::to_string),
        "schedulerDescription": scheduler_description.as_ref().map(ToString::to_string),
    })
}

fn child_overrides(value: &Value) -> Overrides {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(Arc::from);
    Overrides {
        summary_prompt: text("summaryPrompt"),
        eventhub_entry: text("eventhubEntry"),
        eventhub_tool: text("eventhubTool"),
        plugin_configs: value
            .get("pluginConfigs")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        branch_description: text("branchDescription"),
        roster_closing: text("rosterClosing"),
        goal_objective: text("goalObjective"),
        goal_continuation: text("goalContinuation"),
        scheduler_description: text("schedulerDescription"),
    }
}

/// Well-known credential environment names. The preflight fails an isolated
/// run when any of these is visible to an exec child: the run's own
/// credential (the names the model configuration declares) is passed on
/// purpose, everything else on this list is the operator's secret.
const WELL_KNOWN_CREDENTIAL_NAMES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "DEEPSEEK_API_KEY",
    "GEMINI_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "OPENROUTER_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "TOGETHER_API_KEY",
    "XAI_API_KEY",
    "GITHUB_TOKEN",
    "GITLAB_TOKEN",
    "HF_TOKEN",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "GOOGLE_API_KEY",
];

/// The product probe: one Session of the served assembly, created without a
/// mount list, so its model request carries every tool the assembly offers —
/// the host's own tools, the parts' contributed tools and the tools the
/// run's plugin defaults contribute. Its declared set, without the `plugin`
/// tool, is the ceiling every Session of the run mounts, and the
/// tools-product check reads it against `RUN_TOOLS`. Its one turn is answered
/// without a tool call, so the probe never executes anything. It runs under
/// the most restrictive preset, `read-only` — the lowest `PermissionPreset`
/// by `PermissionPreset::rank` — and the permission layer denies file writes
/// outright under it, which costs the tool-free turn nothing.
async fn product_probe(
    client: &mut Client,
    params: &ChildParams,
    database: &Path,
) -> std::result::Result<Vec<(String, ToolSource)>, String> {
    let probe = client
        .rpc(
            "session.create",
            json!({
                "cwd": params.workspace,
                "model": {"provider": "preflight", "modelId": "mock"},
                "permissionPolicy": {"preset": "read-only"},
            }),
        )
        .await
        .map_err(|error| format!("session.create failed: {error}"))?;
    let probe = probe["sessionId"].as_str().unwrap_or_default().to_owned();
    if probe.is_empty() {
        return Err("the product probe Session has no session id".into());
    }
    client
        .rpc("session.attach", json!({"sessionId": probe}))
        .await
        .map_err(|error| format!("session.attach failed: {error}"))?;
    submit(client, &probe, "PRODUCT-TOOLS\n", None)
        .await
        .map_err(|error| error.to_string())?;
    loop {
        match tokio::time::timeout(Duration::from_secs(60), client.next()).await {
            Ok(Ok(WireMessage::Notice { method, params })) => match notice(&method, &params) {
                Notice::Event { event, .. } => {
                    if event["type"] == "turn" && event["name"] == "end" {
                        if let Some(error) = event["data"]["error"].as_str() {
                            return Err(format!("the product probe turn failed: {error}"));
                        }
                        break;
                    }
                }
                Notice::Approval { branch, request } => {
                    let _ = client
                        .rpc(
                            "approval.respond",
                            json!({"sessionId": probe, "branchId": branch,
                                   "requestId": request, "decision": "allow"}),
                        )
                        .await;
                }
                Notice::Other => {}
            },
            Ok(Ok(WireMessage::Response { .. })) => {}
            Ok(Err(error)) => return Err(format!("the product probe Session failed: {error}")),
            Err(_) => return Err("the product probe Session timed out".into()),
        }
    }
    let _ = client
        .rpc("session.close", json!({"sessionId": probe}))
        .await;
    // The declared tools and their sources: the probe's adopted
    // configuration in its exported Timeline. The model request the loopback
    // provider received carries the names; only the Timeline records each
    // tool's `source`.
    let mut export = Vec::new();
    dim_app_server::storage::export_timeline(database.to_path_buf(), &probe, &mut export)
        .await
        .map_err(|error| format!("the product probe Session's export failed: {error}"))?;
    let export = String::from_utf8_lossy(&export).into_owned();
    requested_tools(&export)
}

/// The tools a Session's model request declares, in request order, with
/// each tool's source: the request of the last `model/call` event of an
/// exported Timeline. The request's tools are `ToolDefinition` values
/// (`ModelRequest::tools`), whose `source` — `ToolDefinition::source` — is
/// a required field, so a declared tool without one cannot come from the
/// product: the probe fails, naming the tool, rather than dropping it from
/// the ceiling silently.
fn requested_tools(export: &str) -> std::result::Result<Vec<(String, ToolSource)>, String> {
    let mut declared = None;
    for line in export.lines() {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if line["kind"] != "event" {
            continue;
        }
        let event = &line["event"];
        if event["type"] != "model" || event["name"] != "call" {
            continue;
        }
        let Some(tools) = event["data"]["request"]["tools"].as_array() else {
            continue;
        };
        let mut entries = Vec::with_capacity(tools.len());
        for tool in tools {
            let name = tool["name"]
                .as_str()
                .ok_or_else(|| "a model request tool entry carries no name".to_owned())?
                .to_owned();
            let source = serde_json::from_value(tool["source"].clone()).map_err(|_| {
                format!("the model request declares the tool {name} without a source")
            })?;
            entries.push((name, source));
        }
        declared = Some(entries);
    }
    declared
        .ok_or_else(|| "the product probe Session's Timeline holds no configured tools".to_owned())
}

/// The plugin names the run's default plugin configuration names: the
/// `plugins` and `optional` maps of the defaults file. Empty when the run
/// serves no default plugins or the file cannot be read.
fn plugin_default_names(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for group in ["plugins", "optional"] {
        if let Some(map) = value.get(group).and_then(Value::as_object) {
            names.extend(map.keys().cloned());
        }
    }
    names
}

/// The preflight Session: the product probe, then a scripted Session against
/// the run's preflight provider that checks the run's isolation before the
/// live model's first turn — the disposable `HOME`, the absence of credential
/// names, an outside read refused for `exec` and for a file tool, the
/// operator's home refused for both, and the run's socket unreachable. Any
/// failure aborts the run and is reported as a harness failure.
async fn preflight_session(
    client: &mut Client,
    params: &ChildParams,
    preflight: &Arc<Provider>,
    expected: &[String],
    database: &Path,
) -> std::result::Result<(Vec<PreflightCheck>, Vec<String>), (Vec<PreflightCheck>, String)> {
    let declared = product_probe(client, params, database)
        .await
        .map_err(|error| (Vec::new(), error))?;
    // The ceiling: every tool the served assembly offers, without the
    // `plugin` tool. The probe declares the run's plugin defaults' tools
    // too, so a run that evaluates a plugin still offers the model the thing
    // under test, and the tools-product check reads what the product itself
    // offers against `RUN_TOOLS`.
    let ceiling: Vec<String> = declared
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|name| name != PLUGIN_TOOL)
        .collect();
    let message = format!(
        "PREFLIGHT\nSOCKET={}\nOPERATOR_HOME={}\nOUTPUT_ROOT={}",
        params.socket.display(),
        params
            .operator_home
            .as_ref()
            .map_or("", |home| home.to_str().unwrap_or("")),
        params.outputs.display(),
    );
    let mut create = json!({
        "cwd": params.workspace,
        "model": {"provider": "preflight", "modelId": "mock"},
        "permissionPolicy": {"preset": "full-access"},
    });
    if !params.offer_plugin {
        // The preflight Session's mount list: exactly what the run's
        // Session will declare. The tools check reads the request the
        // loopback provider received and verifies what this Session
        // actually offered.
        create["tools"] = json!(ceiling);
    }
    let created = client
        .rpc("session.create", create)
        .await
        .map_err(|error| (Vec::new(), format!("session.create failed: {error}")))?;
    let session = created["sessionId"].as_str().unwrap_or_default().to_owned();
    if session.is_empty() {
        return Err((Vec::new(), "the preflight Session has no session id".into()));
    }
    client
        .rpc("session.attach", json!({"sessionId": session}))
        .await
        .map_err(|error| (Vec::new(), format!("session.attach failed: {error}")))?;
    submit(client, &session, &message, None)
        .await
        .map_err(|error| (Vec::new(), error))?;
    // The tool results in arrival order: the scripted steps run one per
    // turn, so their order is the check order.
    let mut results: Vec<Value> = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(60), client.next()).await {
            Ok(Ok(WireMessage::Notice { method, params })) => match notice(&method, &params) {
                Notice::Event { event, .. } => {
                    if event["type"] == "step" && event["name"] == "tool-result" {
                        for result in event["data"]["results"]
                            .as_array()
                            .map(Vec::as_slice)
                            .unwrap_or_default()
                        {
                            results.push(result["result"].clone());
                        }
                    }
                    if event["type"] == "turn" && event["name"] == "end" {
                        if let Some(error) = event["data"]["error"].as_str() {
                            return Err((
                                Vec::new(),
                                format!("the preflight turn failed: {error}"),
                            ));
                        }
                        break;
                    }
                }
                Notice::Approval { branch, request } => {
                    let _ = client
                        .rpc(
                            "approval.respond",
                            json!({"sessionId": session, "branchId": branch,
                                   "requestId": request, "decision": "allow"}),
                        )
                        .await;
                }
                Notice::Other => {}
            },
            Ok(Ok(WireMessage::Response { .. })) => {}
            Ok(Err(error)) => {
                return Err((Vec::new(), format!("the preflight Session failed: {error}")));
            }
            Err(_) => return Err((Vec::new(), "the preflight Session timed out".into())),
        }
    }
    let _ = client
        .rpc("session.close", json!({"sessionId": session}))
        .await;
    // The tools the preflight Session's model request carried: the loopback
    // provider received the request before it scripted any step, so its
    // first PREFLIGHT request is this Session's declaration. The product
    // probe's request is the first whose messages carry the probe marker.
    let held = preflight.requests.lock().unwrap();
    let offered = held
        .iter()
        .find_map(|request| {
            if !request.contains("PREFLIGHT") {
                return None;
            }
            let tools = offered_tools(request);
            (!tools.is_empty()).then_some(tools)
        })
        .unwrap_or_default();
    let product = held
        .iter()
        .find_map(|request| {
            if !request.contains("PRODUCT-TOOLS") {
                return None;
            }
            let tools = offered_tools(request);
            (!tools.is_empty()).then_some(tools)
        })
        .unwrap_or_default();
    drop(held);
    let checks = preflight_checks(
        &results, params, &offered, &ceiling, expected, &product, &declared,
    );
    if let Some(failed) = checks.iter().find(|check| !check.passed) {
        let detail = format!("{}: {}", failed.name, failed.detail);
        return Err((checks, detail));
    }
    Ok((checks, ceiling))
}

/// A tool source's name as the checks' detail prints it.
fn source_name(source: ToolSource) -> &'static str {
    match source {
        ToolSource::Static => "static",
        ToolSource::Builtin => "builtin",
        ToolSource::Extension => "extension",
        ToolSource::Dynamic => "dynamic",
    }
}

/// The checks over the preflight Session's tool results and the model
/// request it declared.
fn preflight_checks(
    results: &[Value],
    params: &ChildParams,
    offered: &[String],
    ceiling: &[String],
    expected: &[String],
    product: &[String],
    declared: &[(String, ToolSource)],
) -> Vec<PreflightCheck> {
    let mut checks = Vec::new();
    let mut check = |name: &str, passed: bool, detail: String| {
        checks.push(PreflightCheck {
            name: name.into(),
            passed,
            detail,
        });
    };
    let plugin = offered.iter().any(|tool| tool == PLUGIN_TOOL);
    let exact = offered.len() == ceiling.len()
        && offered
            .iter()
            .zip(ceiling)
            .all(|(offered, ceiling)| offered == ceiling);
    check(
        "tools",
        !plugin && exact,
        if plugin {
            "the preflight Session's model request offered the plugin tool".into()
        } else if exact {
            format!(
                "the preflight Session's model request offered {} tools without the plugin tool",
                offered.len()
            )
        } else {
            format!(
                "the preflight Session's model request offered {} tools, not the run's {}",
                offered.len(),
                ceiling.len()
            )
        },
    );
    // The tool set, by contribution: the native probe child served the same
    // assembly without the plugin defaults, so its declared set N is what
    // the product's own parts contribute, and the run child's set D is N
    // plus what the run's plugin defaults contribute. A tool in N the run
    // list does not name is a native tool the assembly added unreviewed,
    // whatever a defaults file says; a tool in D beyond the run list, the
    // scenario's reply tools and N is the run's plugin defaults'
    // contribution — the harness names those plugins, never the model. A
    // product tool the run list does not name would silently leave a live
    // run without it, and a listed tool the product stopped serving would
    // silently whitelist a name nobody registers; this check catches both
    // at run time, where CI's eval-tests gating cannot hide them. With no
    // named plugin nothing is exempt; with a named plugin a contribution is
    // the defaults' as a whole — the check proves that the defaults name a
    // plugin, not which one contributes the tool. `source` labels the
    // contributed tools in the detail; it is not what decides them.
    let declared_names: BTreeSet<&str> = declared.iter().map(|(name, _)| name.as_str()).collect();
    let expected_set: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    let native: BTreeSet<&str> = params
        .native_tools
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let run_list: BTreeSet<&str> = RUN_TOOLS.iter().copied().collect();
    let unreviewed: Vec<&str> = native
        .difference(&run_list)
        .filter(|name| **name != PLUGIN_TOOL)
        .copied()
        .collect();
    let mut contributed: Vec<(&str, ToolSource)> = Vec::new();
    for (name, source) in declared {
        if name == PLUGIN_TOOL || expected_set.contains(name.as_str()) {
            continue;
        }
        if !native.contains(name.as_str()) {
            contributed.push((name.as_str(), *source));
        }
    }
    let requested: BTreeSet<&str> = product.iter().map(String::as_str).collect();
    let agree = requested == declared_names;
    let absent: Vec<&str> = expected_set.difference(&declared_names).copied().collect();
    let plugins = params
        .defaults
        .as_deref()
        .map(plugin_default_names)
        .unwrap_or_default();
    // Nothing is exempt without a named plugin: the harness keeps no
    // per-tool owner record, so the check can only prove that the defaults
    // name at least one plugin, not which one contributes a tool. With no
    // plugin named, a declared tool beyond the run list, the reply tools
    // and N fails the check; with one named, a contribution is the
    // defaults' as a whole.
    let unowned = plugins.is_empty() && !contributed.is_empty();
    let passed = unreviewed.is_empty() && absent.is_empty() && agree && !unowned;
    // The attributed contributions, in both outcomes: a reader sees what the
    // run's plugin defaults added even when another tool failed the check.
    let contributed_detail = |detail: &mut String| {
        if contributed.is_empty() {
            return;
        }
        if !detail.is_empty() {
            detail.push_str("; ");
        }
        let listed: Vec<String> = contributed
            .iter()
            .map(|(name, source)| format!("{name} ({})", source_name(*source)))
            .collect();
        detail.push_str(&format!(
            "the run's plugin defaults ({}) contribute: {}",
            plugins.join(", "),
            listed.join(", ")
        ));
    };
    check(
        "tools-product",
        passed,
        if passed {
            let mut detail = "the product offered the run's tool set".to_owned();
            contributed_detail(&mut detail);
            detail
        } else {
            let mut detail = String::new();
            if !agree {
                detail.push_str(
                    "the product probe's request and its Timeline declare different tools",
                );
            }
            if !unreviewed.is_empty() {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                detail.push_str(&format!(
                    "the product's native assembly offers tools the run list does not name: {}; update RUN_TOOLS after review",
                    unreviewed.join(", ")
                ));
            }
            if !absent.is_empty() {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                detail.push_str(&format!(
                    "the product's Sessions do not offer tools the run list names: {}",
                    absent.join(", ")
                ));
            }
            if unowned {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                let listed: Vec<String> = contributed
                    .iter()
                    .map(|(name, source)| format!("{name} ({})", source_name(*source)))
                    .collect();
                detail.push_str(&format!(
                    "the defaults name no plugin, so nothing is exempt: {}",
                    listed.join(", ")
                ));
            } else {
                contributed_detail(&mut detail);
            }
            detail
        },
    );
    fn is_error(result: &Value) -> bool {
        result["isError"].as_bool() == Some(true)
    }
    /// The text a tool result carries: the exec output ends with the
    /// command's note, so the checks read it line by line.
    fn text_of(result: &Value) -> String {
        result["content"].as_str().unwrap_or_default().to_owned()
    }
    fn exec(results: &[Value], index: usize) -> Option<&Value> {
        results.get(index)
    }
    /// The credential name a `env` output exposes, or none. The exec
    /// children are what the model's commands see, so any credential name
    /// is a leak: the run's declared name would mean the isolation leaked
    /// the one secret the run exists to keep from the model, and a
    /// well-known name would mean the operator's environment leaked. Shell
    /// startup adds names of its own (`PWD`, `SHLVL`, `_`), which are not
    /// credentials.
    fn credential_name(output: &str, declared: &[String]) -> Option<String> {
        for line in output.lines() {
            let Some((name, _)) = line.split_once('=') else {
                continue;
            };
            // Any credential name is a leak here: the exec children are
            // what the model's commands see, and the sandbox drops every
            // name the run's own server holds. The run's declared name must
            // not appear either — it would mean the isolation leaked the
            // one secret the run exists to keep from the model.
            if declared.iter().any(|declared| declared == name)
                || WELL_KNOWN_CREDENTIAL_NAMES.contains(&name)
            {
                return Some(name.to_owned());
            }
        }
        None
    }
    if let Some(result) = exec(results, 0) {
        let text = text_of(result);
        let printed = text.lines().next().unwrap_or_default().trim();
        check(
            "home",
            !is_error(result) && printed == params.env_home.to_string_lossy(),
            if is_error(result) {
                "echo $HOME was refused".into()
            } else {
                "echo $HOME printed another home".into()
            },
        );
    } else {
        check("home", false, "the preflight saw no echo result".into());
    }
    if let Some(result) = exec(results, 1) {
        let output = text_of(result);
        let offender = credential_name(&output, &params.credentials);
        let detail = match (is_error(result), offender.as_deref()) {
            (true, _) => format!(
                "the env command was refused (exit {}): {}",
                result["structuredContent"]["exitCode"], result["structuredContent"]["code"]
            ),
            (false, Some(name)) => {
                format!("the exec environment showed the credential name {name}")
            }
            (false, None) => "the exec environment carried only the run's own names".into(),
        };
        check(
            "credentials",
            !is_error(result) && offender.is_none(),
            detail,
        );
    } else {
        check(
            "credentials",
            false,
            "the preflight saw no env result".into(),
        );
    }
    if let Some(result) = exec(results, 2) {
        check(
            "exec-outside",
            is_error(result),
            "a command read the marker outside the workspace".into(),
        );
    } else {
        check(
            "exec-outside",
            false,
            "the preflight saw no outside-exec result".into(),
        );
    }
    if let Some(result) = results.get(3) {
        let code = result["structuredContent"]["code"].as_str().unwrap_or("");
        check(
            "read-outside",
            is_error(result) && code == "outside_file_roots",
            "the read tool read the marker outside the workspace".into(),
        );
    } else {
        check(
            "read-outside",
            false,
            "the preflight saw no outside-read result".into(),
        );
    }
    if let Some(result) = exec(results, 4) {
        check(
            "socket",
            is_error(result),
            "a command reached the run's socket".into(),
        );
    } else {
        check(
            "socket",
            false,
            "the preflight saw no socket probe result".into(),
        );
    }
    if let Some(result) = exec(results, 5) {
        check(
            "exec-operator-home",
            is_error(result),
            "a command read the operator's home".into(),
        );
    } else {
        check(
            "exec-operator-home",
            false,
            "the preflight saw no operator-home exec result".into(),
        );
    }
    if let Some(result) = results.get(6) {
        let code = result["structuredContent"]["code"].as_str().unwrap_or("");
        check(
            "read-operator-home",
            is_error(result) && code == "outside_file_roots",
            "the read tool read the operator's home".into(),
        );
    } else {
        check(
            "read-operator-home",
            false,
            "the preflight saw no operator-home read result".into(),
        );
    }
    if let Some(result) = exec(results, 7) {
        check(
            "exec-outputs",
            is_error(result),
            "a command read the marker under the outputs root".into(),
        );
    } else {
        check(
            "exec-outputs",
            false,
            "the preflight saw no outputs-root exec result".into(),
        );
    }
    if let Some(result) = results.get(8) {
        let code = result["structuredContent"]["code"].as_str().unwrap_or("");
        check(
            "read-outputs",
            is_error(result) && code == "outside_file_roots",
            "the read tool read the marker under the outputs root".into(),
        );
    } else {
        check(
            "read-outputs",
            false,
            "the preflight saw no outputs-root read result".into(),
        );
    }
    checks
}

#[allow(clippy::too_many_arguments)]
async fn run_once(
    options: &Options,
    arm: &'static str,
    overrides: &Overrides,
    scenario: &Scenario,
    run: usize,
    cancel: &CancellationToken,
    native: &[(String, ToolSource)],
    instrument: &mut Instrument,
) -> Result<RunRecord> {
    let run_directory = run_root(options, arm, scenario, run);
    if run_directory.exists() {
        return Err(format!(
            "{} exists; give a fresh --out for a new evaluation",
            run_directory.display()
        )
        .into());
    }
    // The run root: the workspace and the disposable environment
    // directories, and nothing else.
    private_directory(&run_directory)?;
    copy_tree(
        &scenario.directory.join("workspace"),
        &run_directory.join("workspace"),
    )?;
    let env_home = run_directory.join("env/home");
    private_directory(&env_home)?;
    private_directory(&run_directory.join("env/tmp"))?;
    private_directory(&run_directory.join("env/xdg-config"))?;
    private_directory(&run_directory.join("env/xdg-cache"))?;
    private_directory(&run_directory.join("env/xdg-data"))?;
    // The marker the preflight's outside probe reads; it sits in the run
    // root, outside the workspace, so only an unisolated run can read it.
    std::fs::write(run_directory.join("preflight-outside"), "outside marker")?;
    // The marker the preflight's output-root probe reads; it sits under
    // the outputs root, where the judge views and the sibling runs'
    // records live, outside the boundary's roots, so only an unisolated
    // run can read it.
    let outputs_root = outputs_root(options, arm, scenario);
    private_directory(&outputs_root)?;
    std::fs::write(outputs_root.join("preflight-outside"), "outside marker")?;
    // The run's Dim home: fresh, under the output root, never the
    // operator's `--home`.
    let run_home = options
        .out
        .join("homes")
        .join(arm)
        .join(&scenario.id)
        .join(format!("run-{run}"));
    private_directory(&run_home)?;
    // The run's endpoint: under the output root, outside the run root. A
    // Unix socket's path is bounded (SUN_LEN), so a deep --out binds the
    // socket in a short directory and leaves the out-relative name as a
    // link the layout still owns.
    let socket = run_socket(options, arm, scenario, run);
    private_directory(socket.parent().expect("a socket has a parent"))?;
    let socket = bindable_socket(&socket)?;
    let (record, outcome) = isolate_run(
        options,
        arm,
        overrides,
        scenario,
        run,
        cancel,
        native,
        &run_home,
        &workspace_of(&run_directory),
        &env_home,
        &socket,
        instrument,
    )
    .await?;
    write_run_outputs(options, arm, scenario, run, &outcome)?;
    Ok(record)
}

/// The workspace path of a fresh run root.
fn workspace_of(run_directory: &Path) -> PathBuf {
    run_directory.join("workspace")
}

/// The run root of one run: the workspace and the disposable environment
/// directories under the output root, and nothing else.
fn run_root(options: &Options, arm: &str, scenario: &Scenario, run: usize) -> PathBuf {
    options
        .out
        .join(arm)
        .join(&scenario.id)
        .join(format!("run-{run}"))
}

/// The run's socket under the output root, outside the run root: the
/// model's commands reach only what the sandbox and the file-root
/// boundary grant, so the endpoint the harness drives must lie outside
/// the workspace and the run root the workspace lives in.
fn run_socket(options: &Options, arm: &str, scenario: &Scenario, run: usize) -> PathBuf {
    options
        .out
        .join("sockets")
        .join(format!("{arm}-{}-run-{run}.sock", scenario.id))
}

/// The run's own account files, under the output root, never inside the run
/// root: the export and the view the judge read. A re-judge reads them from
/// here ([`read_saved_runs`]).
fn write_run_outputs(
    options: &Options,
    arm: &'static str,
    scenario: &Scenario,
    run: usize,
    outcome: &ChildOutcome,
) -> Result<()> {
    let outputs = outputs_root(options, arm, scenario);
    private_directory(&outputs)?;
    if let Some(export) = &outcome.export {
        std::fs::write(outputs.join(format!("run-{run}.timeline.jsonl")), export)?;
    }
    if let Some(view) = &outcome.view {
        std::fs::write(outputs.join(format!("run-{run}.judge-view.jsonl")), view)?;
    }
    // The run root keeps only the workspace and the environment, so the
    // preflight report lives beside the run's account files.
    std::fs::write(
        outputs.join(format!("run-{run}.preflight.json")),
        serde_json::to_string_pretty(&outcome.preflight)?,
    )?;
    Ok(())
}

/// Starts the run's serve child: its own process, `env_clear`ed, with the
/// disposable home, `PATH` and the model configuration's one credential
/// variable, and nothing else of the operator's environment. The child
/// prints one JSON line; the parent reads it and builds the record.
#[allow(clippy::too_many_arguments)]
async fn isolate_run(
    options: &Options,
    arm: &'static str,
    overrides: &Overrides,
    scenario: &Scenario,
    run: usize,
    cancel: &CancellationToken,
    native: &[(String, ToolSource)],
    run_home: &Path,
    workspace: &Path,
    env_home: &Path,
    socket: &Path,
    instrument: &mut Instrument,
) -> Result<(RunRecord, ChildOutcome)> {
    // The child serves the run's own preflight provider on its loopback
    // and adds it to the run's model configuration; the parent only names
    // the configuration the operator gave the evaluation.
    let operator_home = std::env::var_os("HOME").map(PathBuf::from);
    let boundary = run_boundary(options, workspace, env_home)?;
    let credentials = augmented_credentials(options);
    let params = ChildParams {
        home: run_home.to_owned(),
        workspace: workspace.to_owned(),
        env_home: env_home.to_owned(),
        output_root: options.out.join("command-output"),
        socket: socket.to_owned(),
        model_config: options.model_config.clone().ok_or(
            "an evaluation needs --model-config: the model configuration \
             names the model under test and the credential its calls use",
        )?,
        defaults: options.defaults.clone(),
        node: options.node.clone(),
        js_runtime: options.js_runtime.clone(),
        overrides: overrides_value(overrides),
        scenario: serde_json::to_value(scenario)?,
        model: options.model.clone(),
        judge: options.judge.clone(),
        boundary,
        outputs: outputs_root(options, arm, scenario),
        credentials,
        operator_home,
        isolate: options.isolate,
        fail_preflight: options.fail_preflight,
        offer_plugin: options.offer_plugin,
        extra_tool: options.extra_tool,
        fail_child: options
            .fail_child
            .as_ref()
            .filter(|(arm_name, _)| arm_name == arm)
            .map(|(_, marker)| marker.clone()),
        fail_child_holds_stdout: options.fail_child_holds_stdout,
        native_tools: native.to_vec(),
        probe_only: false,
    };
    let params_path = options
        .out
        .join("children")
        .join(arm)
        .join(&scenario.id)
        .join(format!("run-{run}.json"));
    private_directory(params_path.parent().expect("a params file has a parent"))?;
    std::fs::write(&params_path, serde_json::to_string_pretty(&params)?)?;
    let outcome = spawn_child(options, &params_path, &params, cancel).await?;
    let record = child_record(arm, scenario, run, &outcome, instrument)?;
    Ok((record, outcome))
}

/// The file-tool root boundary of a run's sessions: the workspace (writes),
/// the disposable home and the platform's fixed roots (reads); `None` when
/// the run is not isolated (the offline ablation).
fn run_boundary(
    options: &Options,
    workspace: &Path,
    env_home: &Path,
) -> Result<Option<FileToolBoundary>> {
    if !options.isolate {
        return Ok(None);
    }
    let mut read_only_roots = platform_roots();
    read_only_roots.sort();
    read_only_roots.dedup();
    // The read tools compare canonical paths, so the roots must be
    // canonical too: `/var` and its firmlink `/private/var` resolve to one
    // real path, and a non-canonical root would refuse everything under it.
    let boundary = FileToolBoundary {
        workspace: workspace.to_owned(),
        home: env_home.to_owned(),
        read_only_roots,
    }
    .canonicalize()
    .map_err(|error| format!("cannot canonicalize the run's file roots: {error}"))?;
    Ok(Some(boundary))
}

/// The differential native probe: one child served exactly as a run child is
/// served, but without the plugin defaults, whose probe Session declares the
/// assembly's native tool set. Its set N tells each run's `tools-product`
/// check which declared tools the product's own parts contribute: a tool in
/// N beyond `RUN_TOOLS` fails the check, and a tool beyond both the run list
/// and N is what the run's plugin defaults added.
async fn native_probe(
    options: &Options,
    cancel: &CancellationToken,
) -> Result<Vec<(String, ToolSource)>> {
    let root = options.out.join("native-probe");
    let home = options.out.join("homes").join("native-probe");
    let socket = options.out.join("sockets").join("native-probe.sock");
    // The probe starts from clean directories and a fresh endpoint: a
    // reused --out would otherwise let an earlier probe's database or
    // workspace answer for this one, and the declared set would depend on
    // that history.
    for path in [&root, &home] {
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
    }
    let _ = std::fs::remove_file(&socket);
    private_directory(&root)?;
    let workspace = root.join("workspace");
    private_directory(&workspace)?;
    let env_home = root.join("env/home");
    private_directory(&env_home)?;
    for directory in ["tmp", "xdg-config", "xdg-cache", "xdg-data"] {
        private_directory(&root.join("env").join(directory))?;
    }
    private_directory(&home)?;
    private_directory(socket.parent().expect("a socket has a parent"))?;
    let socket = bindable_socket(&socket)?;
    let params = ChildParams {
        home,
        workspace: workspace.clone(),
        env_home: env_home.clone(),
        output_root: options.out.join("command-output"),
        socket,
        model_config: options.model_config.clone().ok_or(
            "an evaluation needs --model-config: the model configuration \
             names the model under test and the credential its calls use",
        )?,
        // The one difference from a run child: no plugin defaults, so the
        // probe Session declares the assembly's native tools only.
        defaults: None,
        node: None,
        js_runtime: None,
        overrides: json!({}),
        scenario: json!({}),
        model: options.model.clone(),
        judge: options.judge.clone(),
        boundary: run_boundary(options, &workspace, &env_home)?,
        outputs: root.clone(),
        credentials: augmented_credentials(options),
        operator_home: std::env::var_os("HOME").map(PathBuf::from),
        isolate: options.isolate,
        fail_preflight: false,
        offer_plugin: false,
        extra_tool: options.extra_tool,
        fail_child: None,
        fail_child_holds_stdout: false,
        native_tools: Vec::new(),
        probe_only: true,
    };
    let params_path = root.join("params.json");
    std::fs::write(&params_path, serde_json::to_string_pretty(&params)?)?;
    let outcome = spawn_child_json(options, &params_path, &params, cancel, "nativeTools").await?;
    serde_json::from_value(outcome["nativeTools"].clone())
        .map_err(|error| format!("the native probe printed no tool set: {error}").into())
}

/// The endpoint path the run's server binds: the out-relative socket when
/// its path fits a Unix socket address, else a short directory under the
/// system temporary directory with the out-relative name linked to it, so
/// the run's layout keeps the socket under the output root either way. Only
/// unix has the address length limit and the link; a non-unix run refuses
/// before a run starts.
fn bindable_socket(out_relative: &Path) -> Result<PathBuf> {
    if out_relative.as_os_str().len() <= 96 {
        return Ok(out_relative.to_owned());
    }
    #[cfg(unix)]
    {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "dim-eval-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        private_directory(&directory)?;
        let socket = directory.join("rpc.sock");
        let _ = std::fs::remove_file(out_relative);
        std::os::unix::fs::symlink(&socket, out_relative).map_err(|error| {
            format!("cannot link the socket {}: {error}", out_relative.display())
        })?;
        Ok(socket)
    }
    #[cfg(not(unix))]
    {
        Err("scenario-eval needs a unix socket".into())
    }
}

/// The platform's fixed read-only roots, as the sandbox and the file-tool
/// boundary share them (ADR 014): the one list lives in the capabilities
/// crate, and this expands its requested paths and canonical forms.
fn platform_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in dim_agent_capabilities::host_platform::PLATFORM_ROOTS {
        let path = PathBuf::from(path);
        if !out.contains(&path) {
            out.push(path.clone());
        }
        if let Ok(canonical) = std::fs::canonicalize(&path)
            && !out.contains(&canonical)
        {
            out.push(canonical);
        }
    }
    out
}

/// The run's model configuration with the run's own preflight provider
/// added: the operator's file plus a `preflight` provider that points at
/// the loopback the child serves, written beside the run's params.
fn augment_model_config(source: &Path, preflight_url: &str, target: &Path) -> Result<()> {
    private_directory(target.parent().expect("a model configuration has a parent"))?;
    let bytes = std::fs::read(source)?;
    let mut file: Value = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "Invalid model configuration {} at line {}, column {}",
            source.display(),
            error.line(),
            error.column()
        )
    })?;
    file["providers"]
        .as_array_mut()
        .ok_or("the model configuration has no providers list")?
        .push(json!({
            "id": "preflight",
            "kind": "openai-chat",
            "baseUrl": preflight_url,
            "models": [{
                "id": "mock",
                "contextWindow": PREFLIGHT_WINDOW,
                "maxOutputTokens": PREFLIGHT_OUTPUT,
                "config": {"maxTokens": PREFLIGHT_OUTPUT}
            }]
        }));
    std::fs::write(target, serde_json::to_string_pretty(&file)? + "\n")?;
    Ok(())
}

/// The outputs root of one run's arm: where the run's account files, the
/// judge views and the sibling runs' records live.
fn outputs_root(options: &Options, arm: &str, scenario: &Scenario) -> PathBuf {
    options.out.join("outputs").join(arm).join(&scenario.id)
}

/// The preflight provider's declared window and output limit.
const PREFLIGHT_WINDOW: u64 = 24_000;
const PREFLIGHT_OUTPUT: u64 = 200;

/// The credential names the run's model configuration reads: the operator's
/// environment names the parent passes the child, and the child passes
/// nothing else of the operator's environment through.
fn augmented_credentials(options: &Options) -> Vec<String> {
    match options
        .model_config
        .as_ref()
        .and_then(|config| dim_app_server::cli::model_profile(config).ok())
    {
        Some((models, _)) => models.api_key_envs().into_iter().collect(),
        None => Vec::new(),
    }
}

/// Serves a run's preflight provider on a loopback endpoint, and returns
/// the endpoint URL and the provider whose request list the checks read.
async fn serve_preflight() -> Result<(String, Arc<Provider>, tokio::task::JoinHandle<()>)> {
    let provider = Arc::new(Provider {
        requests: Mutex::default(),
        reply: preflight_reply,
        hold: holds_nothing,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = listener.local_addr()?;
    let app = axum::Router::new()
        .route("/v1/chat/completions", axum::routing::post(completion))
        .with_state(provider.clone());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Ok((format!("http://{endpoint}/v1"), provider, handle))
}

/// The command line of the run's serve child: the runner's own binary — the
/// `scenario-eval-serve` subcommand in the product binary, and the
/// `eval_server_child` test function when the runner is a test binary.
fn child_command(params_path: &Path) -> (PathBuf, Vec<OsString>, bool) {
    let executable = std::env::current_exe().expect("the runner has its own binary");
    // The runner's own binary: `xtask` (the product binary) or a test
    // binary `xtask-<hash>` (the suite's bin tests). The test binary's
    // `eval_server_child` entry is the child; the product binary's
    // `scenario-eval-serve` subcommand is.
    let is_test_binary = executable
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with("xtask-"));
    let args = if is_test_binary {
        vec![
            "--exact".into(),
            "scenario_eval::tests::eval_server_child".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ]
    } else {
        vec!["scenario-eval-serve".into(), params_path.as_os_str().into()]
    };
    (executable, args, is_test_binary)
}

/// The child's environment: `env_clear`, then the disposable home and the
/// few names a server needs. The ablation (isolation off) inherits the
/// operator's whole environment, as the runner served runs before this
/// work.
fn child_environment(
    options: &Options,
    params: &ChildParams,
    params_path: &Path,
    test_binary: bool,
) -> std::io::Result<Vec<(OsString, OsString)>> {
    let mut env = Vec::new();
    if !options.isolate {
        let mut env = std::env::vars_os().collect::<Vec<_>>();
        if test_binary {
            env.push((
                OsString::from("DIM_EVAL_SERVE_PARAMS"),
                params_path.as_os_str().to_owned(),
            ));
        }
        return Ok(env);
    }
    let mut passthrough = |name: &str| {
        if let Some(value) = std::env::var_os(name) {
            env.push((OsString::from(name), value));
        }
    };
    // The names a run's child keeps from the operator's environment: the
    // globals its sessions' processes need. `PATH`, `LANG`, `LC_ALL`,
    // `LC_CTYPE` and `TERM` are how a shell and its tools run.
    // `DEVELOPER_DIR`: the sandbox's developer-directory read root comes
    // from it (host_platform/isolation.rs `baseline_read_paths`).
    // `NO_COLOR`: no reader in the child; its exec sessions force `1`.
    // `PAGER`: no reader in the child; its exec sessions force `cat`.
    // `GIT_PAGER`: no reader in the child; its exec sessions force `cat`.
    // `GH_PAGER`: no reader in the child; its exec sessions force `cat`.
    for name in [
        "PATH",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "NO_COLOR",
        "PAGER",
        "GIT_PAGER",
        "GH_PAGER",
        #[cfg(target_os = "macos")]
        "DEVELOPER_DIR",
    ] {
        passthrough(name);
    }
    for (name, value) in [
        ("HOME", params.env_home.as_os_str()),
        (
            "TMPDIR",
            params.env_home.parent().unwrap().join("tmp").as_os_str(),
        ),
        (
            "XDG_CONFIG_HOME",
            params
                .env_home
                .parent()
                .unwrap()
                .join("xdg-config")
                .as_os_str(),
        ),
        (
            "XDG_CACHE_HOME",
            params
                .env_home
                .parent()
                .unwrap()
                .join("xdg-cache")
                .as_os_str(),
        ),
        (
            "XDG_DATA_HOME",
            params
                .env_home
                .parent()
                .unwrap()
                .join("xdg-data")
                .as_os_str(),
        ),
    ] {
        env.push((OsString::from(name), value.to_owned()));
    }
    for name in &params.credentials {
        if let Some(value) = std::env::var_os(name) {
            env.push((OsString::from(name), value));
        }
    }
    if test_binary {
        env.push((
            OsString::from("DIM_EVAL_SERVE_PARAMS"),
            params_path.as_os_str().to_owned(),
        ));
    }
    Ok(env)
}

/// Spawns the run's serve child and reads its outcome line.
async fn spawn_child(
    options: &Options,
    params_path: &Path,
    params: &ChildParams,
    cancel: &CancellationToken,
) -> Result<ChildOutcome> {
    let outcome = spawn_child_json(options, params_path, params, cancel, "judgeOutcome").await?;
    Ok(serde_json::from_value(outcome)?)
}

/// The serve children this test binary started, by their params path: a
/// test finds the child another run's failure must have taken down.
#[cfg(test)]
static SPAWNED: Mutex<Vec<(PathBuf, u32)>> = Mutex::new(Vec::new());

/// Spawns a child and reads the JSON object of the output line that names
/// `marker`: a run's child prints its outcome under `judgeOutcome`, the
/// native probe's child its declared tool set under `nativeTools`.
async fn spawn_child_json(
    options: &Options,
    params_path: &Path,
    params: &ChildParams,
    cancel: &CancellationToken,
    marker: &str,
) -> Result<Value> {
    let (executable, args, test_binary) = child_command(params_path);
    let env = child_environment(options, params, params_path, test_binary)?;
    let mut command = tokio::process::Command::new(&executable);
    command
        .args(&args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        // The child is the run's only owner: dropped or cancelled, it must
        // not outlive the harness, or it keeps calling the model and
        // writing under --out after the report is written.
        .kill_on_drop(true);
    // The child leads its own process group, so the abort path takes the
    // members left in it with it — the MCP stdio servers, and the git, npm
    // and pip children, all spawned without a group of their own. Not a
    // runtime host: rutis starts it with a stdin it does not inherit, and
    // that spawn puts it in a group of its own (rutis-bridge pipe/spawn.rs).
    // Not a shell's exec children either: they setsid. What ends a runtime
    // host is its channel closing; rutis's own tests show a Node host exit
    // by itself once its host's channel does.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start the run's serve child: {error}"))?;
    #[cfg(test)]
    SPAWNED
        .lock()
        .unwrap()
        .push((params_path.to_owned(), child.id().unwrap_or_default()));
    #[cfg(unix)]
    let mut group = ChildGroup::new(child.id().unwrap_or_default());
    // Drain the pipe while the child runs: the child may print more than a
    // pipe holds (the run's logged output rides the same stream), and a full
    // pipe would block the child's last print forever, so waiting and then
    // reading would deadlock.
    let reading = child.stdout.take().map(|mut stdout| {
        tokio::spawn(async move {
            let mut output = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut output)
                .await
                .map_err(|error| format!("cannot read the serve child's outcome: {error}"))?;
            Ok::<Vec<u8>, String>(output)
        })
    });
    let status = tokio::select! {
        result = child.wait() => {
            let status = result?;
            // The group is killed before the output is read: a member the
            // failed child left behind may hold the write end of its stdout,
            // and the reader would wait for an EOF that never comes. A
            // successful child left no member behind, so its group is
            // disarmed instead.
            #[cfg(unix)]
            if status.success() {
                group.disarm();
            } else {
                group.kill();
            }
            status
        }
        _ = cancel.cancelled() => {
            #[cfg(unix)]
            group.kill();
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("the evaluation was cancelled".into());
        }
    };
    let output = match reading {
        Some(reading) => reading
            .await
            .map_err(|error| format!("the outcome reader failed: {error}"))??,
        None => Vec::new(),
    };
    let text = String::from_utf8_lossy(&output);
    if !status.success() {
        // The group was killed as soon as the exit status was known: a
        // member left in it may hold this child's stdout, and the read
        // above would never see EOF. An empty group answered ESRCH and the
        // kill ignored it.
        // A test hook's child names the event that ended it on a line of
        // its own; the error carries that name, so a test waiting for the
        // event fails when it never arrived instead of reading only the
        // exit status.
        if let Some(message) = text
            .lines()
            .filter_map(|line| line.find('{').map(|start| &line[start..]))
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|value| value["failChild"].as_str().map(str::to_owned))
        {
            return Err(format!("the run's serve child exited with {status}: {message}").into());
        }
        return Err(format!("the run's serve child exited with {status}").into());
    }
    let line = text
        .lines()
        .find(|line| line.contains(marker))
        .ok_or_else(|| {
            format!(
                "the run's serve child printed no {marker} line: {}",
                text.trim()
            )
        })?;
    // A test-binary child runs under the test harness, which opens its
    // output line with the test's name; the outcome is the rest of the
    // line.
    let json = &line[line
        .find('{')
        .ok_or("the child's outcome line carries no JSON")?..];
    Ok(serde_json::from_str(json)?)
}

/// The serve child's process group: the child leads it (`process_group(0)`),
/// so signalling it takes the members left in it. Armed until the child
/// exits on its own; dropping the guard kills whatever the group still
/// holds.
#[cfg(unix)]
struct ChildGroup {
    pid: u32,
    armed: bool,
}

#[cfg(unix)]
impl ChildGroup {
    fn new(pid: u32) -> Self {
        Self { pid, armed: true }
    }

    /// The child exited on its own: its group is not the harness's to kill.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Kills every process in the group, once.
    fn kill(&mut self) {
        if self.armed {
            self.armed = false;
            if self.pid != 0 {
                // SAFETY: the child leads the group its own `process_group
                // (0)` created, so the negated id signals only the group the
                // child leads.
                unsafe { libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL) };
            }
        }
    }
}

#[cfg(unix)]
impl Drop for ChildGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The record of a finished run: the child's outcome with the configs the
/// report reads merged into the instrument.
fn child_record(
    arm: &'static str,
    scenario: &Scenario,
    run: usize,
    outcome: &ChildOutcome,
    instrument: &mut Instrument,
) -> Result<RunRecord> {
    let stop: &'static str = match outcome.stop.as_str() {
        "idle" => "idle",
        "turns" => "turns",
        "seconds" => "seconds",
        _ => "error",
    };
    let error = match &outcome.harness {
        Some(harness) => Some(format!("harness failure: {harness}")),
        None => outcome.error.clone(),
    };
    let record = RunRecord {
        arm,
        scenario: scenario.id.clone(),
        run,
        session: outcome.session.clone(),
        stop,
        turns: outcome.turns,
        window: outcome.window,
        duration_ms: outcome.duration_ms,
        failed: failed(stop, error.as_deref(), outcome.judge_outcome),
        judge_outcome: outcome.judge_outcome,
        error,

        judge_error: outcome.judge_error.clone(),
        judge_reasks: outcome.judge_reasks,
        judge_calls: outcome.judge_calls.clone(),
        temperature_dropped: Some(outcome.temperature_dropped),
        decline_retries: Some(outcome.decline_retries),
        events: outcome.events.clone(),
        reply_calls: outcome.reply_calls.clone(),
        probes: outcome
            .probes
            .iter()
            .map(|probe| {
                let kind: &'static str = match probe.kind.as_deref() {
                    Some("must_show") => "must_show",
                    Some("must_not_show") => "must_not_show",
                    _ => {
                        return Err(format!(
                            "the run's record carries an unknown probe kind: {:?}",
                            probe.kind
                        )
                        .into());
                    }
                };
                Ok(ProbeRecord {
                    id: probe.id.clone(),
                    kind,
                    verdict: probe.verdict,
                    quote: probe.quote.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    instrument.model_configuration = instrument.model_configuration.take().or_else(|| {
        outcome
            .model_configuration
            .clone()
            .map(|(effort, tier)| Configuration { effort, tier })
    });
    instrument.judge_configuration = instrument.judge_configuration.take().or_else(|| {
        outcome
            .judge_configuration
            .clone()
            .map(|(effort, tier)| Configuration { effort, tier })
    });
    if instrument.judge_window.is_none() {
        instrument.judge_window = Some(JudgeWindow {
            window: outcome.judge_window.0,
            max_output: outcome.judge_window.1,
        });
    }
    Ok(record)
}

/// What one run left behind: the driven Session, its export, the view of it/// What one run left behind: the driven Session, its export, the view of it
/// the judge read, and the judge's part.
struct Finished {
    driven: Driven,
    export: Option<String>,
    view: Option<String>,
    judging: Judging,
}
impl Finished {
    fn failed(scenario: &Scenario, error: String) -> Self {
        Self {
            driven: Driven {
                session: String::new(),
                stop: "error",
                turns: 0,
                window: None,
                decline_retries: 0,
                error: Some(error),
            },
            export: None,
            view: None,
            judging: Judging::not_asked(scenario, None),
        }
    }
}

/// The judge's part of one run: how its calls ended, the verdict for every
/// probe, its error and what its calls cost.
struct Judging {
    /// How the judge's calls ended (model-prompts §8.1).
    outcome: JudgeOutcome,
    /// One record per probe: the judge's verdicts, or every probe `unparsed`.
    probes: Vec<ProbeRecord>,
    /// The judge's error; `None` when its verdicts are usable.
    error: Option<String>,
    /// How many times the judge was asked again for an answer it could not
    /// use.
    reasks: u32,
    /// One entry per judge call, in order.
    calls: Vec<JudgeCall>,
    /// Whether the judge had to drop its temperature.
    temperature_dropped: bool,
}
impl Judging {
    /// A run the judge was never asked about: it left no export, the profile
    /// has no model host for the judge, or the run failed before the judge
    /// ran.
    fn not_asked(scenario: &Scenario, error: Option<String>) -> Self {
        Self {
            outcome: JudgeOutcome::NotAsked,
            probes: unparsed(scenario),
            error,
            reasks: 0,
            calls: Vec::new(),
            temperature_dropped: false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive_once(
    server: &RunningServer,
    settings: &RunSettings,
    run_home: &Path,
    workspace: &Path,
    scenario: &Scenario,
    model: &ModelSelection,
    judge: &ModelSelection,
    limits: JudgeWindow,
    cancel: &CancellationToken,
    producer: &ProducerState,
    tools: Option<Vec<String>>,
) -> Result<Finished> {
    let socket = settings
        .unix_path
        .clone()
        .ok_or("a run serves its App Server on a local endpoint")?;
    let mut client = Client::connect(&ConnectionConfig {
        endpoint: socket,
        server: WireServer {
            program: std::env::current_exe()?,
            embedded: true,
        },
        profile: None,
        client_kind: "other",
        client_name: "scenario-eval",
        protocol_version: 4,
    })
    .await
    .map_err(|error| format!("cannot connect to the served App Server: {error}"))?;
    if settings.defaults.is_some() {
        wait_for_plugins(&mut client).await?;
    }
    let mut driven = drive(&mut client, scenario, workspace, model, producer, tools).await;
    if driven.session.is_empty() {
        return Ok(Finished {
            driven,
            export: None,
            view: None,
            judging: Judging::not_asked(scenario, None),
        });
    }
    let _ = client
        .rpc("session.close", json!({"sessionId": driven.session}))
        .await;
    drop(client);
    let mut export = Vec::new();
    let export = match dim_app_server::storage::export_timeline(
        settings.database(run_home),
        &driven.session,
        &mut export,
    )
    .await
    {
        Ok(()) => {
            let export = String::from_utf8_lossy(&export).into_owned();
            let (turns, failed_turn) = turns_in_export(&export);
            driven.turns = turns;
            if let Some(error) = failed_turn
                && driven.error.is_none()
            {
                driven.stop = "error";
                driven.error = Some(error);
            }
            export
        }
        Err(error) => {
            let error = error.to_string();
            return Ok(Finished {
                driven,
                export: None,
                view: None,
                judging: Judging::not_asked(scenario, Some(format!("no export to judge: {error}"))),
            });
        }
    };
    let (Some(host), Some(resolver)) = (
        server.service::<ModelHost>(),
        server.service::<ProviderConnectionResolver>(),
    ) else {
        return Ok(Finished {
            driven,
            export: Some(export),
            view: None,
            judging: Judging::not_asked(
                scenario,
                Some("the served profile has no model host for the judge".to_owned()),
            ),
        });
    };
    let view = judge_view(&export);
    let judging = judge_export(&host, &resolver, judge, scenario, &view, limits, cancel).await;
    Ok(Finished {
        driven,
        export: Some(export),
        view: Some(view),
        judging,
    })
}

/// Whether the run is a failed run (model-prompts §8.1): it failed on its
/// own, an error or its wall-clock budget, or its judging did not happen — a
/// judge call failed, there was no export to judge, or no model host for the
/// judge.
fn failed(stop: &str, error: Option<&str>, judge_outcome: JudgeOutcome) -> bool {
    stop == "seconds"
        || error.is_some()
        || matches!(
            judge_outcome,
            JudgeOutcome::CallFailed | JudgeOutcome::NotAsked
        )
}

/// Every probe of a run that was not judged.
fn unparsed(scenario: &Scenario) -> Vec<ProbeRecord> {
    scenario
        .probes
        .iter()
        .map(|probe| ProbeRecord {
            id: probe.id.clone(),
            kind: probe.kind.name(),
            verdict: ProbeVerdict::Unparsed,
            quote: None,
        })
        .collect()
}

/// The run's turns as the exported Timeline records them, and the first failed
/// turn's error. The export is the account of record: a run that ends on a
/// budget or an error stops reading the notice stream with notices still
/// queued, so that stream can hold fewer turn ends than the Timeline does.
/// Turns cancelled by Timeline closure are excluded.
fn turns_in_export(export: &str) -> (u32, Option<String>) {
    let mut turns = 0;
    let mut failed = None;
    for line in export.lines() {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if line["kind"] != "event" {
            continue;
        }
        let event = &line["event"];
        if event["type"] != "turn" || event["name"] != "end" {
            continue;
        }
        if event["data"]["status"] == "cancelled" && event["data"]["reason"] == "timeline closed" {
            continue;
        }
        turns += 1;
        if failed.is_none() && event["data"]["status"] == "failed" {
            failed = Some(format!(
                "{}: the turn failed: {}",
                line["branch_id"].as_str().unwrap_or("main"),
                event["data"]["error"].as_str().unwrap_or("no error given")
            ));
        }
    }
    (turns, failed)
}

pub(crate) async fn selection(
    server: &RunningServer,
    target: &ModelId,
    cwd: &Path,
    cancel: &CancellationToken,
) -> Result<ModelSelection> {
    let host = server
        .service::<ModelSelectionHost>()
        .ok_or("the served profile resolves no model selection")?;
    host.0
        .resolve(
            Some(target.clone()),
            Some(cwd.to_path_buf()),
            cancel.clone(),
        )
        .await
        .map_err(|error| format!("{}/{}: {error}", target.provider_id, target.model_id).into())
}

/// Waits until the default plugin rows leave `pending`: they start after
/// the server is up, and a `calls` step or a plugin's text needs its row
/// loaded. A failed row fails the run and names itself.
async fn wait_for_plugins(client: &mut Client) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let items = client
            .rpc("plugin.list", json!({}))
            .await
            .map_err(|error| format!("plugin.list failed: {error}"))?;
        let items = items
            .as_array()
            .ok_or("plugin.list did not answer a list")?;
        let mut pending = false;
        for item in items {
            let name = item["name"].as_str().unwrap_or_default();
            match item["status"].as_str().unwrap_or_default() {
                "failed" => {
                    let error = item["error"].as_str().unwrap_or("no reason given");
                    return Err(format!("the plugin {name} failed to load: {error}").into());
                }
                "pending" => pending = true,
                _ => {}
            }
        }
        if !pending {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("the default plugins did not start within 60 seconds".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// What one driven run reached.
struct Driven {
    session: String,
    stop: &'static str,
    turns: u32,
    window: Option<f64>,
    /// How many declines the runner sent again after `question_not_pending`.
    decline_retries: u32,
    error: Option<String>,
}

/// Why a Session whose window is not the scenario's cannot be measured, or `None`.
fn window_mismatch(window: Option<f64>, declared: u64) -> Option<String> {
    (window != Some(declared as f64)).then(|| {
        format!(
            "the Session declares a context window of {}, and the scenario declares {declared}",
            window.map_or_else(|| "none".to_owned(), |window| window.to_string())
        )
    })
}

/// Drives one Session through the App Server's client methods, as a client
/// does: create, attach, submit the script, and wait for idle or a budget.
/// `tools` is the Session's mount list; `None` is the tools-check
/// ablation, which leaves the mount list unset so the Session offers the
/// `plugin` tool.
async fn drive(
    client: &mut Client,
    scenario: &Scenario,
    workspace: &Path,
    model: &ModelSelection,
    producer: &ProducerState,
    tools: Option<Vec<String>>,
) -> Driven {
    if !scenario.sources.is_empty()
        && let Err(error) = declare_sources(producer, &scenario.sources).await
    {
        return Driven {
            session: String::new(),
            stop: "error",
            turns: 0,
            window: None,
            decline_retries: 0,
            error: Some(error),
        };
    }
    let mut create = json!({
        "cwd": workspace,
        "model": {"provider": model.provider_id, "modelId": model.model_id},
        "permissionPolicy": {"preset": "full-access"},
    });
    if let Some(tools) = &tools {
        // The run's mount list (config.rs `SessionConfig::tools`).
        create["tools"] = json!(tools);
    }
    let created = client.rpc("session.create", create).await;
    let session = match created {
        Ok(reply) => reply["sessionId"].as_str().unwrap_or_default().to_owned(),
        Err(error) => {
            return Driven {
                session: String::new(),
                stop: "error",
                turns: 0,
                window: None,
                decline_retries: 0,
                error: Some(format!("session.create failed: {error}")),
            };
        }
    };
    let mut driven = Driven {
        session: session.clone(),
        stop: "idle",
        turns: 0,
        window: None,
        decline_retries: 0,
        error: None,
    };
    if let Err(error) = client
        .rpc("session.attach", json!({"sessionId": session}))
        .await
    {
        driven.stop = "error";
        driven.error = Some(format!("session.attach failed: {error}"));
        return driven;
    }
    // The Session serves its window only once a model request has counted the context.
    // A run that cannot read it here reads it again when it settles, if a request ran.
    let mut window_deferred = false;
    match client
        .rpc(
            "session.context.usage",
            json!({"sessionId": session, "branchId": "main"}),
        )
        .await
    {
        Ok(usage) => {
            let window = usage["contextWindow"].as_f64();
            driven.window = window;
            if let Some(error) = window_mismatch(window, scenario.budget.window) {
                driven.stop = "error";
                driven.error = Some(error);
                return driven;
            }
        }
        Err(error) if dim_client::app_error(&error) == Some("context_model_failed") => {
            window_deferred = true;
        }
        Err(error) => {
            driven.stop = "error";
            driven.error = Some(format!(
                "the Session serves no context window to check the scenario's budget against: {error}"
            ));
            return driven;
        }
    }
    for (index, call) in scenario.calls.iter().enumerate() {
        let mut params = match &call.params {
            Value::Object(params) => params.clone(),
            _ => serde_json::Map::new(),
        };
        params.insert("sessionId".to_owned(), json!(session));
        if let Err(error) = client.rpc(&call.method, Value::Object(params)).await {
            driven.stop = "error";
            driven.error = Some(format!(
                "the calls step {} ({}) failed: {error}",
                index + 1,
                call.method
            ));
            return driven;
        }
    }
    for source in &scenario.sources {
        let Some(subscribe) = &source.subscribe else {
            // The scenario expects the model to subscribe through the
            // `events` tool; the event trigger waits for that subscription.
            continue;
        };
        let mut params = json!({
            "types": subscribe.types.clone(),
            "target": {"sessionId": session, "branchId": "main"},
        });
        if let Some(mode) = subscribe.mode {
            params["mode"] = json!(mode);
        }
        if let Err(error) = client.rpc("eventhub.subscribe", params).await {
            driven.stop = "error";
            driven.error = Some(format!(
                "the subscription of {} failed: {error}",
                source.name
            ));
            return driven;
        }
    }
    if let Err(error) = submit(client, &session, &scenario.message, None).await {
        driven.stop = "error";
        driven.error = Some(error);
        return driven;
    }
    let started = Instant::now();
    let mut branches: BTreeSet<String> = ["main".to_owned()].into();
    let mut steers: Vec<(Steer, bool)> = scenario
        .steers
        .iter()
        .cloned()
        .map(|steer| (steer, false))
        .collect();
    let mut last_event = Instant::now();
    // The events still to publish, and whether each was.
    let mut events: Vec<(EventSpec, bool)> = scenario
        .events
        .iter()
        .cloned()
        .map(|event| (event, false))
        .collect();
    // An input the runner submitted is outstanding until a turn starts for it
    // or the run consumes it; a line that has done neither is not idle.
    let mut awaiting = true;
    // The declines whose `ask_user` call the Session had not registered when
    // they were sent, kept until the call's `step/tool-result` closes it.
    let mut declines: Vec<Decline> = Vec::new();
    // The idle requests the quiet check keeps outstanding, one per line.
    let mut calls = IdleCalls::default();
    loop {
        match fire_due(client, &session, &mut steers, started).await {
            Ok(submitted) => awaiting |= submitted,
            Err(error) => {
                driven.stop = "error";
                driven.error = Some(error);
                return driven;
            }
        }
        match fire_events_due(client, producer, &mut events, started).await {
            Ok((published, starts)) => {
                awaiting |= starts;
                if published {
                    last_event = Instant::now();
                }
            }
            Err(error) => {
                driven.stop = "error";
                driven.error = Some(error);
                return driven;
            }
        }
        match fire_events_after_subscribe(client, producer, &mut events).await {
            Ok((published, starts)) => {
                awaiting |= starts;
                if published {
                    last_event = Instant::now();
                }
            }
            Err(error) => {
                driven.stop = "error";
                driven.error = Some(error);
                return driven;
            }
        }
        if let Err(error) =
            retry_declines(client, &session, &mut declines, &mut driven.decline_retries).await
        {
            driven.stop = "error";
            driven.error = Some(error);
            return driven;
        }
        // An `afterSubscribedSeconds` event whose clock has not started is
        // not pending: only a subscription of the model's can start it, so a
        // model that never subscribed cannot make it fire, and the run ends
        // idle with the event never published. The clock check above ran
        // first in this pass, so a subscription the model made in its last
        // turn has already started the clock and keeps the run open here
        // (EventHub design A.4).
        let pending = steers.iter().any(|(_, fired)| !fired)
            || events.iter().any(|(event, fired)| {
                !fired
                    && (event.after_subscribed_seconds.is_none() || event.subscribed_at.is_some())
            });
        if started.elapsed().as_secs_f64() >= scenario.budget.seconds {
            driven.stop = "seconds";
            break;
        }
        if driven.turns >= scenario.budget.turns {
            driven.stop = "turns";
            break;
        }
        if !pending && !awaiting && last_event.elapsed() >= QUIET {
            match idle(client, &session, &branches, &mut calls).await {
                Ok(true) => break,
                Ok(false) => last_event = Instant::now(),
                Err(error) => {
                    driven.stop = "error";
                    driven.error = Some(error);
                    return driven;
                }
            }
        }
        let wait = wait_time(&steers, &events, started, &scenario.budget);
        match tokio::time::timeout(wait, client.next()).await {
            Ok(Ok(WireMessage::Notice { method, params })) => {
                last_event = Instant::now();
                match notice(&method, &params) {
                    Notice::Event { branch, event } => {
                        if let Some(sequence) = event["sequence"].as_u64() {
                            calls.read(&branch, sequence);
                        }
                        if event["type"] == "branch"
                            && event["name"] == "create"
                            && let Some(branch) = event["data"]["branch"].as_str()
                        {
                            branches.insert(branch.to_owned());
                        }
                        // A steer is taken inside the turn it reaches, so no
                        // turn starts for it: either event means the input is
                        // no longer outstanding.
                        if (event["type"] == "turn" && event["name"] == "start")
                            || (event["type"] == "input" && event["name"] == "consumed")
                        {
                            awaiting = false;
                        }
                        if event["type"] == "turn" && event["name"] == "end" {
                            driven.turns += 1;
                            let status = event["data"]["status"]
                                .as_str()
                                .or_else(|| event["data"]["kind"].as_str());
                            if status == Some("failed") {
                                driven.stop = "error";
                                driven.error = Some(format!(
                                    "{branch}: the turn failed: {}",
                                    event["data"]["error"].as_str().unwrap_or("no error given")
                                ));
                                return driven;
                            }
                        }
                        match fire_triggered(client, &session, &mut steers, &event).await {
                            Ok(submitted) => awaiting |= submitted,
                            Err(error) => {
                                driven.stop = "error";
                                driven.error = Some(error);
                                return driven;
                            }
                        }
                        match fire_events_triggered(client, producer, &mut events, &event).await {
                            Ok((published, starts)) => {
                                awaiting |= starts;
                                if published {
                                    last_event = Instant::now();
                                }
                            }
                            Err(error) => {
                                driven.stop = "error";
                                driven.error = Some(error);
                                return driven;
                            }
                        }
                        if let Err(error) =
                            decline_question(client, &session, &mut declines, &branch, &event).await
                        {
                            driven.stop = "error";
                            driven.error = Some(error);
                            return driven;
                        }
                    }
                    Notice::Approval { branch, request } => {
                        let _ = client
                            .rpc(
                                "approval.respond",
                                json!({"sessionId": session, "branchId": branch,
                                       "requestId": request, "decision": "allow"}),
                            )
                            .await;
                    }
                    Notice::Other => {}
                }
            }
            Ok(Ok(WireMessage::Response { id, result })) => {
                // A reply the quiet check left outstanding is read here the
                // way the check reads it: the check could wait for it no
                // longer, and the line's state still owes its answer.
                let branch = branches
                    .iter()
                    .find(|branch| calls.outstanding(branch) == Some(id))
                    .cloned();
                if let Some(branch) = branch
                    && let Err(error) = calls.reply(&branch, result.map_err(Into::into))
                {
                    driven.stop = "error";
                    driven.error = Some(format!("{branch}: {error}"));
                    return driven;
                }
            }
            Ok(Err(error)) => {
                driven.stop = "error";
                driven.error = Some(format!("the Session connection failed: {error}"));
                return driven;
            }
            Err(_) => {}
        }
    }
    if driven.stop == "turns" || driven.stop == "seconds" {
        for branch in &branches {
            let _ = client
                .rpc(
                    "session.stop",
                    json!({"sessionId": session, "branchId": branch,
                           "reason": format!("budget: {}", driven.stop)}),
                )
                .await;
        }
    }
    if window_deferred
        && driven.error.is_none()
        && let Ok(usage) = client
            .rpc(
                "session.context.usage",
                json!({"sessionId": session, "branchId": "main"}),
            )
            .await
    {
        let window = usage["contextWindow"].as_f64();
        driven.window = window;
        if let Some(error) = window_mismatch(window, scenario.budget.window) {
            driven.stop = "error";
            driven.error = Some(error);
        }
    }
    driven
}

/// A question the runner declined that the Session has no waiter for yet. The
/// Questions capability registers a call's waiter inside the tool handler
/// (Questions §3.1), so a decline that arrives before the handler ran is
/// `question_not_pending`: the client keeps its prompt until the call's
/// `step/tool-result` arrives and lets the user answer again (§3.2, A12).
pub(crate) struct Decline {
    branch: String,
    request: String,
    call: String,
    /// When the decline was last sent: the retry waits a quiet period, so a
    /// busy line's notices cannot multiply the requests.
    sent: Instant,
}

/// Declines the `ask_user` calls of a `step/tool-call`, keeps the declines the
/// Session does not wait for yet, and drops the calls a `step/tool-result`
/// settled. A decline for a call the Session did not take stays with the run
/// until [`retry_declines`] answers it.
pub(crate) async fn decline_question(
    client: &mut Client,
    session: &str,
    declines: &mut Vec<Decline>,
    branch: &str,
    event: &Value,
) -> std::result::Result<(), String> {
    match (event["type"].as_str(), event["name"].as_str()) {
        (Some("step"), Some("tool-call")) => {
            // `ToolCallData.request_id` is required, so a question's
            // `step/tool-call` always carries the batch it answers: a call
            // without one cannot be keyed to any reply.
            let request = event["data"]["request_id"].as_str().unwrap_or_default();
            let calls = event["data"]["calls"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            for call in calls {
                if call["name"] != "ask_user" {
                    continue;
                }
                let call_id = call["id"].as_str().unwrap_or_default();
                // `ToolCallData.calls[].id` is as required as `request_id`:
                // the decline is keyed by both, so a call without an id
                // cannot be keyed to any reply either.
                if call_id.is_empty() {
                    return Err(format!("{branch}: the ask_user call carries no id"));
                }
                if request.is_empty() {
                    return Err(format!(
                        "{branch}: the ask_user call {call_id} carries no request_id"
                    ));
                }
                let decline = Decline {
                    branch: branch.to_owned(),
                    request: request.to_owned(),
                    call: call_id.to_owned(),
                    sent: Instant::now(),
                };
                if !decline_once(client, session, &decline).await? {
                    declines.push(decline);
                }
            }
        }
        (Some("step"), Some("tool-result")) => {
            let request = event["data"]["request_id"].as_str().unwrap_or_default();
            let settled: Vec<&str> = event["data"]["results"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|result| result["call_id"].as_str())
                .collect();
            declines.retain(|decline| {
                decline.branch != branch
                    || decline.request != request
                    || !settled.contains(&decline.call.as_str())
            });
        }
        _ => {}
    }
    Ok(())
}

/// Answers the declines again that the Session did not take, at most once per
/// quiet period since the last attempt: the quiet period wakes the driver at
/// least that often, so the registration is never missed, and a line that
/// keeps delivering other notices does not multiply the requests. The caller
/// owns the count, so a run that ends on a reply still holds the resends it
/// sent before it.
pub(crate) async fn retry_declines(
    client: &mut Client,
    session: &str,
    declines: &mut Vec<Decline>,
    sent: &mut u32,
) -> std::result::Result<(), String> {
    let mut index = 0;
    while index < declines.len() {
        if declines[index].sent.elapsed() < QUIET {
            index += 1;
            continue;
        }
        declines[index].sent = Instant::now();
        if decline_once(client, session, &declines[index]).await? {
            declines.remove(index);
        } else {
            index += 1;
        }
        *sent += 1;
    }
    Ok(())
}

/// One `session.question.respond` decline: `true` when the call took it, and
/// `false` while the Session has no waiter for the call. Any other reply ends
/// the run.
async fn decline_once(
    client: &mut Client,
    session: &str,
    decline: &Decline,
) -> std::result::Result<bool, String> {
    let result = client
        .rpc(
            "session.question.respond",
            json!({"sessionId": session, "branchId": decline.branch,
                   "requestId": decline.request, "callId": decline.call,
                   "declined": true}),
        )
        .await;
    // The product's Questions client classifies this reply the same way
    // (`dim_client::question::reply`); a transport error that is no RPC reply
    // can only be a failure.
    let reply = match result {
        Ok(value) => dim_client::question::reply(&Ok(value)),
        Err(error) => match error.downcast::<dim_client::RpcError>() {
            Ok(error) => dim_client::question::reply(&Err(*error)),
            Err(error) => dim_client::question::Reply::Failed(error.to_string()),
        },
    };
    match reply {
        dim_client::question::Reply::Accepted => Ok(true),
        dim_client::question::Reply::NotPending => Ok(false),
        // The line keeps a reply it could not take, and lets the user send
        // the answer again; a run has no user to resend, and the call would
        // wait out the budget, so the run ends on the error where the CLI
        // keeps the prompt.
        dim_client::question::Reply::Rejected(reason) => Err(format!(
            "{}: the decline of {} failed: {reason}",
            decline.branch, decline.call
        )),
        dim_client::question::Reply::Failed(error) => Err(format!(
            "{}: the decline of {} failed: {error}",
            decline.branch, decline.call
        )),
    }
}

/// One reply-tool call the run's tools recorded, in the order they arrived.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReplyCall {
    pub(crate) tool: String,
    pub(crate) message_id: String,
    pub(crate) text: String,
}

/// One event a run published, and the receipt EventHub answered (EventHub
/// design A.2).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventRecord {
    pub(crate) source: String,
    pub(crate) r#type: String,
    pub(crate) id: String,
    pub(crate) receipt: Value,
}

/// The scenario producer's state: its row's context and the `eventHub`
/// service, as its `apply` or an `eventhub/ready` notice last found them,
/// plus what the run's reply tools and publishes recorded.
#[derive(Default)]
pub(crate) struct ProducerState {
    installed: Mutex<Option<(Ctx, Option<Arc<EventHub>>)>>,
    reply_calls: Mutex<Vec<ReplyCall>>,
    events: Mutex<Vec<EventRecord>>,
}

impl ProducerState {
    fn install(&self, ctx: Ctx, hub: Option<Arc<EventHub>>) {
        *self.installed.lock().unwrap() = Some((ctx, hub));
    }
    pub(crate) fn hub(&self) -> Option<Arc<EventHub>> {
        self.installed
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|(_, hub)| hub.clone())
    }
    fn ctx(&self) -> std::result::Result<Ctx, String> {
        self.installed
            .lock()
            .unwrap()
            .as_ref()
            .map(|(ctx, _)| ctx.clone())
            .ok_or_else(|| "the scenario producer row has not applied".to_owned())
    }
    async fn declare(&self, request: DeclareRequest) -> std::result::Result<(), String> {
        let hub = self
            .hub()
            .ok_or_else(|| "eventHub is not installed".to_owned())?;
        hub.declare(&self.ctx()?, request)
            .await
            .map_err(|error| format!("{}: {}", error.code, error.message))
    }
    /// Publishes one event through the service and records its receipt. A
    /// delivery that is not `delivered` fails the run; a model's missed
    /// `afterSubscribedSeconds` event is recorded and starts no turn. Answers
    /// whether any delivered branch starts a turn (EventHub design §3.1,
    /// A.2).
    async fn publish(
        &self,
        event: &EventSpec,
        subscriptions: &BTreeMap<String, Option<InputMode>>,
    ) -> std::result::Result<bool, String> {
        let hub = self
            .hub()
            .ok_or_else(|| "eventHub is not installed".to_owned())?;
        let request = PublishRequest {
            source: event.source.clone(),
            r#type: event.r#type.clone(),
            id: Some(event.id.clone()),
            title: event.title.clone(),
            body: event.body.clone(),
            attributes: event.attributes.clone(),
            to: Vec::new(),
        };
        let receipt = hub
            .publish(&self.ctx()?, request)
            .await
            .map_err(|error| format!("event {}: {}: {}", event.id, error.code, error.message))?;
        self.events.lock().unwrap().push(EventRecord {
            source: event.source.clone(),
            r#type: event.r#type.clone(),
            id: event.id.clone(),
            receipt: receipt.clone(),
        });
        let deliveries = receipt["deliveries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if deliveries.is_empty() {
            // A model's subscription owns an `afterSubscribedSeconds` event:
            // the publish matched nobody because the model left no
            // subscription, and that miss is the outcome the probes judge,
            // not a harness failure (EventHub design A.3, A.4).
            if event.after_subscribed_seconds.is_some() {
                return Ok(false);
            }
            return Err(format!(
                "event {}: the receipt is not delivered: {receipt}",
                event.id
            ));
        }
        if deliveries
            .iter()
            .any(|delivery| delivery["status"] != json!("delivered"))
        {
            return Err(format!(
                "event {}: the receipt is not delivered: {receipt}",
                event.id
            ));
        }
        // A delivered receipt carries no mode (EventHub design §3.1), so a
        // delivery's mode is the strongest effective mode among the
        // subscriptions it lists: an id names one the snapshot took just
        // before the publish, its own `mode` or else the type's declared
        // mode.
        let mut starts = false;
        for delivery in &deliveries {
            let mode = delivery["subscriptionIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|id| {
                    subscriptions
                        .get(id)
                        .copied()
                        .flatten()
                        .unwrap_or(event.declared_mode)
                })
                .max_by_key(|mode| mode_rank(*mode))
                // A branch reached through no subscription takes the type's
                // declared mode.
                .unwrap_or(event.declared_mode);
            starts |= starts_turn(mode);
        }
        Ok(starts)
    }
    pub(crate) fn events(&self) -> Vec<EventRecord> {
        self.events.lock().unwrap().clone()
    }
    pub(crate) fn reply_calls(&self) -> Vec<ReplyCall> {
        self.reply_calls.lock().unwrap().clone()
    }
    fn record_reply(&self, tool: &str, message_id: String, text: String) {
        self.reply_calls.lock().unwrap().push(ReplyCall {
            tool: tool.to_owned(),
            message_id,
            text,
        });
    }
}

/// Whether a delivery starts a turn: `append` joins the context without one
/// (EventHub design §3.1).
fn starts_turn(mode: InputMode) -> bool {
    mode != InputMode::Append
}

/// The declaration a scenario's source makes (EventHub design A.1).
fn declaration(source: &Source) -> DeclareRequest {
    DeclareRequest {
        source: source.name.clone(),
        guide: source.guide.clone(),
        types: source
            .types
            .iter()
            .map(|declared| TypeDeclaration {
                r#type: declared.r#type.clone(),
                description: declared.description.clone(),
                mode: declared.mode,
            })
            .collect(),
    }
}

/// Installs the one producer row a scenario runs with: it reads the
/// `eventHub` service optionally, declares every source, and registers the
/// sources' reply tools for the Session (EventHub design §2.1’s producer
/// pattern; the tool registration follows
/// `crates/dim-agent-scheduler/src/session.rs`).
pub(crate) fn install_producer(
    builder: &mut AgentBuilder,
    sources: &[Source],
    state: Arc<ProducerState>,
) {
    let sources = sources.to_vec();
    builder.plugin(move |names| {
        Ok(Box::new(ScenarioProducer::new(
            names,
            sources.clone(),
            state.clone(),
        )?) as Box<dyn DynPlugin>)
    });
}

/// A test hook: a Session part that contributes one tool `RUN_TOOLS` does
/// not name, from a part no default plugin names, so a test can watch the
/// tools-product check refuse it.
fn install_extra_tool(builder: &mut AgentBuilder) {
    builder.plugin(|names| Ok(Box::new(ExtraToolProducer::new(names)?) as Box<dyn DynPlugin>));
}

struct ExtraToolProducer {
    parts: Key<PartRegistry>,
}

impl ExtraToolProducer {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            parts: names.key()?,
        })
    }
}

impl DynPlugin for ExtraToolProducer {
    fn name(&self) -> &str {
        "ExtraToolProducer"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.require_at(&self.parts)?.register(
                ctx,
                PartScope::Session,
                "ExtraToolSession",
                |mount| Ok(Some(Box::new(ExtraToolSession::new(mount.names)?))),
            )?;
            Ok(Effect::Done)
        })
    }
}

struct ExtraToolSession {
    tools: Key<ToolRegistry>,
}

impl ExtraToolSession {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            tools: names.key()?,
        })
    }
}

impl DynPlugin for ExtraToolSession {
    fn name(&self) -> &str {
        "ExtraToolSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tools = ctx.require_at(&self.tools)?;
            let mut extra = definition(
                "fixture_extra",
                "A test hook's tool outside the run list.",
                json!({"type": "object"}),
            );
            extra.source = ToolSource::Extension;
            tools.register(
                ctx,
                ToolContribution {
                    definition: extra,
                    handler: Arc::new(ExtraTool),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

struct ExtraTool;

impl ToolHandler for ExtraTool {
    fn execute(
        &self,
        _call: ToolCall,
        _context: ToolExecutionContext,
        _cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move { Ok(ToolResult::text("extra")) })
    }
}

struct ScenarioProducer {
    state: Arc<ProducerState>,
    hub: Key<EventHub>,
    parts: Key<PartRegistry>,
    sources: Vec<Source>,
}

impl ScenarioProducer {
    fn new(
        names: &Names<'_>,
        sources: Vec<Source>,
        state: Arc<ProducerState>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            state,
            hub: names.key()?,
            parts: names.key()?,
            sources,
        })
    }
}

impl DynPlugin for ScenarioProducer {
    fn name(&self) -> &str {
        "ScenarioProducer"
    }
    fn injects(&self) -> Vec<TypeKey> {
        // The Session part registry gates the row; `eventHub` itself is read
        // optionally with `get_at` and heard through `eventhub/ready`
        // (EventHub design §2.1).
        vec![self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let state = self.state.clone();
            let key = self.hub.clone();
            // The notice first, then the read: a service provided between the
            // two steps is not missed (EventHub design §2.1).
            {
                let state = state.clone();
                let key = key.clone();
                let listen_ctx = ctx.clone();
                ctx.listen::<dim_event_hub::Ready>().on(move |_, _| {
                    let state = state.clone();
                    let key = key.clone();
                    let ctx = listen_ctx.clone();
                    async move {
                        let hub = ctx.get_at(&key);
                        state.install(ctx.clone(), hub);
                        Ok(None)
                    }
                })?;
            }
            let hub = ctx.get_at(&key);
            state.install(ctx.clone(), hub);
            let tools: Vec<String> = self
                .sources
                .iter()
                .filter_map(|source| source.reply_tool.clone())
                .collect();
            let reply_state = self.state.clone();
            ctx.require_at(&self.parts)?.register(
                ctx,
                PartScope::Session,
                "ScenarioProducerSession",
                move |mount| {
                    Ok(Some(Box::new(ScenarioReplySession::new(
                        mount.names,
                        tools.clone(),
                        reply_state.clone(),
                    )?)))
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

/// The producer's Session part: the reply tools the scenario's sources name.
struct ScenarioReplySession {
    tools: Key<ToolRegistry>,
    tool_names: Vec<String>,
    state: Arc<ProducerState>,
}

impl ScenarioReplySession {
    fn new(
        names: &Names<'_>,
        tool_names: Vec<String>,
        state: Arc<ProducerState>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            tools: names.key()?,
            tool_names,
            state,
        })
    }
}

impl DynPlugin for ScenarioReplySession {
    fn name(&self) -> &str {
        "ScenarioProducerSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tools = ctx.require_at(&self.tools)?;
            for name in &self.tool_names {
                tools.register(
                    ctx,
                    ToolContribution {
                        definition: reply_definition(name),
                        handler: Arc::new(ReplyTool {
                            name: name.clone(),
                            state: self.state.clone(),
                        }),
                    },
                )?;
            }
            Ok(Effect::Done)
        })
    }
}

/// The reply tool a scenario's source names: it takes `{messageId, text}`,
/// records the call in the run's record and answers `sent`.
struct ReplyTool {
    name: String,
    state: Arc<ProducerState>,
}

impl ToolHandler for ReplyTool {
    fn execute(
        &self,
        call: ToolCall,
        _context: ToolExecutionContext,
        _cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            let input = Value::Object(call.input);
            let Some(message_id) = input["messageId"].as_str() else {
                return Ok(ToolResult::error(format!(
                    "{}: invalid_params\nmessageId is required",
                    self.name
                )));
            };
            let Some(text) = input["text"].as_str() else {
                return Ok(ToolResult::error(format!(
                    "{}: invalid_params\ntext is required",
                    self.name
                )));
            };
            self.state
                .record_reply(&self.name, message_id.to_owned(), text.to_owned());
            Ok(ToolResult::text("sent"))
        })
    }
}

/// A reply tool's declaration.
fn reply_definition(name: &str) -> ToolDefinition {
    let mut tool = definition(
        name,
        "Replies to a message from this source, by the message's id.",
        json!({
            "type": "object",
            "required": ["messageId", "text"],
            "additionalProperties": false,
            "properties": {
                "messageId": {"type": "string", "description": "The id attribute of the event message being answered."},
                "text": {"type": "string", "description": "The reply to send."}
            }
        }),
    );
    tool.source = ToolSource::Extension;
    tool
}

/// Declares every source through the producer row before the Session
/// subscribes: the subscription checks the catalog, and the service appears
/// once EventHub applies (EventHub design §2.1, A.1, A.3).
async fn declare_sources(
    producer: &ProducerState,
    sources: &[Source],
) -> std::result::Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while producer.hub().is_none() {
        if Instant::now() >= deadline {
            return Err("the eventHub service is not installed".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for source in sources {
        producer
            .declare(declaration(source))
            .await
            .map_err(|error| format!("source {}: {error}", source.name))?;
    }
    Ok(())
}

/// The snapshot a publish's receipt is read against: each subscription's own
/// `mode`, absent when the type's declared mode applies (EventHub design
/// §3.1, A.2), read with the management call `eventhub.subscriptions` just
/// before the publish.
async fn subscription_modes(
    client: &mut Client,
) -> std::result::Result<BTreeMap<String, Option<InputMode>>, String> {
    let reply = client
        .rpc("eventhub.subscriptions", json!({}))
        .await
        .map_err(|error| format!("eventhub.subscriptions failed: {error}"))?;
    let mut snapshot = BTreeMap::new();
    for subscription in reply["subscriptions"].as_array().into_iter().flatten() {
        let Some(id) = subscription["id"].as_str() else {
            continue;
        };
        let mode = subscription["mode"]
            .as_str()
            .and_then(|mode| serde_json::from_value(json!(mode)).ok());
        snapshot.insert(id.to_owned(), mode);
    }
    Ok(snapshot)
}

/// Publishes the timed events that are due; answers whether it published one
/// and whether one starts a turn.
async fn fire_events_due(
    client: &mut Client,
    producer: &ProducerState,
    events: &mut [(EventSpec, bool)],
    started: Instant,
) -> std::result::Result<(bool, bool), String> {
    let mut published = false;
    let mut starts = false;
    for (event, fired) in events.iter_mut() {
        let due = event
            .after_seconds
            .is_some_and(|after| started.elapsed().as_secs_f64() >= after);
        if due && !*fired {
            *fired = true;
            let snapshot = subscription_modes(client).await?;
            starts |= producer.publish(event, &snapshot).await?;
            published = true;
        }
    }
    Ok((published, starts))
}

/// Publishes the events whose `whenToolCall` substring the notice's
/// `step/tool-use` event carries; answers whether it published one and
/// whether one starts a turn.
async fn fire_events_triggered(
    client: &mut Client,
    producer: &ProducerState,
    events: &mut [(EventSpec, bool)],
    notice: &Value,
) -> std::result::Result<(bool, bool), String> {
    if notice["type"] != "step" || notice["name"] != "tool-use" {
        return Ok((false, false));
    }
    let data = notice["data"].to_string();
    let mut published = false;
    let mut starts = false;
    for (event, fired) in events.iter_mut() {
        let triggered = event
            .when_tool_call
            .as_ref()
            .is_some_and(|trigger| data.contains(trigger.as_str()));
        if triggered && !*fired {
            *fired = true;
            let snapshot = subscription_modes(client).await?;
            starts |= producer.publish(event, &snapshot).await?;
            published = true;
        }
    }
    Ok((published, starts))
}

/// Whether a subscription's `types` name `source`: an exact type of the
/// source or its `<source>.*` pattern (EventHub design A.3).
fn subscription_names_source(subscription: &Value, source: &str) -> bool {
    subscription["types"].as_array().is_some_and(|types| {
        types.iter().any(|pattern| {
            pattern
                .as_str()
                .is_some_and(|pattern| match pattern.strip_suffix(".*") {
                    Some(pattern_source) => pattern_source == source,
                    None => pattern.split_once('.').map(|(source, _)| source) == Some(source),
                })
        })
    })
}

/// Whether an `afterSubscribedSeconds` event is due: that many seconds after
/// the runner first saw a subscription naming its source.
fn subscribed_due(event: &EventSpec) -> bool {
    event.after_subscribed_seconds.is_some_and(|after| {
        event
            .subscribed_at
            .is_some_and(|at| at.elapsed().as_secs_f64() >= after)
    })
}

/// The mode rank of EventHub design §3.1: `steer` over `queue` over `append`.
fn mode_rank(mode: InputMode) -> u8 {
    match mode {
        InputMode::Steer => 2,
        InputMode::Queue => 1,
        InputMode::Append => 0,
    }
}

/// Starts the clock of every `afterSubscribedSeconds` event whose source now
/// has a subscription, and publishes the events whose clock has run out; it
/// answers whether it published one and whether one starts a turn. The
/// scenario's own sources carry no subscription here: the model creates it
/// through the `events` tool (EventHub design A.4). A delivery's mode comes
/// from the subscriptions the receipt lists, taken from a snapshot of each
/// subscription's own `mode` read just before the publish (EventHub design
/// §3.1, A.2).
async fn fire_events_after_subscribe(
    client: &mut Client,
    producer: &ProducerState,
    events: &mut [(EventSpec, bool)],
) -> std::result::Result<(bool, bool), String> {
    let needs_first = events.iter().any(|(event, fired)| {
        !*fired && event.after_subscribed_seconds.is_some() && event.subscribed_at.is_none()
    });
    if needs_first {
        let reply = client
            .rpc("eventhub.subscriptions", json!({}))
            .await
            .map_err(|error| format!("eventhub.subscriptions failed: {error}"))?;
        let subscriptions = reply["subscriptions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let now = Instant::now();
        for (event, fired) in events.iter_mut() {
            if *fired || event.after_subscribed_seconds.is_none() || event.subscribed_at.is_some() {
                continue;
            }
            if subscriptions
                .iter()
                .any(|subscription| subscription_names_source(subscription, &event.source))
            {
                event.subscribed_at = Some(now);
            }
        }
    }
    let due = events
        .iter()
        .any(|(event, fired)| !*fired && subscribed_due(event));
    let snapshot = if due {
        subscription_modes(client).await?
    } else {
        BTreeMap::new()
    };
    let mut published = false;
    let mut starts = false;
    for (event, fired) in events.iter_mut() {
        if !*fired && subscribed_due(event) {
            *fired = true;
            starts |= producer.publish(event, &snapshot).await?;
            published = true;
        }
    }
    Ok((published, starts))
}

/// The `session.submit` params. A mode names how `main` takes the input: a
/// steer ends a wait and replaces a request that has produced no output yet,
/// while the operator's first message carries no mode and is queued.
fn submit_params(session: &str, text: &str, mode: Option<InputMode>) -> Value {
    let mut params = json!({"sessionId": session, "branchId": "main", "text": text});
    if let Some(mode) = mode {
        params["mode"] = json!(mode);
    }
    params
}

async fn submit(
    client: &mut Client,
    session: &str,
    text: &str,
    mode: Option<InputMode>,
) -> std::result::Result<(), String> {
    client
        .rpc("session.submit", submit_params(session, text, mode))
        .await
        .map(|_| ())
        .map_err(|error| format!("session.submit failed: {error}"))
}

/// Sends the timed steers that are due, and whether it sent one.
async fn fire_due(
    client: &mut Client,
    session: &str,
    steers: &mut [(Steer, bool)],
    started: Instant,
) -> std::result::Result<bool, String> {
    let mut sent = false;
    for (steer, fired) in steers.iter_mut() {
        let due = steer
            .after_seconds
            .is_some_and(|after| started.elapsed().as_secs_f64() >= after);
        if due && !*fired {
            *fired = true;
            submit(client, session, &steer.text, Some(InputMode::Steer)).await?;
            sent = true;
        }
    }
    Ok(sent)
}

/// Sends the event-triggered steers whose substring the notice's
/// `step/tool-use` event carries, and whether it sent one.
async fn fire_triggered(
    client: &mut Client,
    session: &str,
    steers: &mut [(Steer, bool)],
    event: &Value,
) -> std::result::Result<bool, String> {
    if event["type"] != "step" || event["name"] != "tool-use" {
        return Ok(false);
    }
    let data = event["data"].to_string();
    let mut sent = false;
    for (steer, fired) in steers.iter_mut() {
        let triggered = steer
            .when_tool_call
            .as_ref()
            .is_some_and(|trigger| data.contains(trigger.as_str()));
        if triggered && !*fired {
            *fired = true;
            submit(client, session, &steer.text, Some(InputMode::Steer)).await?;
            sent = true;
        }
    }
    Ok(sent)
}

/// How long to wait for the next notice: until the next timed steer, the
/// quiet check or the wall-clock budget, whichever comes first.
fn wait_time(
    steers: &[(Steer, bool)],
    events: &[(EventSpec, bool)],
    started: Instant,
    budget: &Budget,
) -> Duration {
    let mut wait = QUIET;
    let until = |seconds: f64| {
        let left = seconds - started.elapsed().as_secs_f64();
        Duration::from_secs_f64(left.max(0.001))
    };
    wait = wait.min(until(budget.seconds));
    for (steer, fired) in steers {
        if let (Some(after), false) = (steer.after_seconds, *fired) {
            wait = wait.min(until(after));
        }
    }
    for (event, fired) in events {
        if let (Some(after), false) = (event.after_seconds, *fired) {
            wait = wait.min(until(after));
        }
        // Before the first subscription the quiet period is the poll; after
        // it, the clock is exact.
        if let (Some(after), Some(at), false) =
            (event.after_subscribed_seconds, event.subscribed_at, *fired)
        {
            let left = after - at.elapsed().as_secs_f64();
            wait = wait.min(Duration::from_secs_f64(left.max(0.001)));
        }
    }
    wait.max(Duration::from_millis(10))
}

enum Notice {
    Event { branch: String, event: Value },
    Approval { branch: String, request: String },
    Other,
}
fn notice(method: &str, params: &Value) -> Notice {
    match method {
        "session.event" => Notice::Event {
            branch: params["branchId"].as_str().unwrap_or("main").to_owned(),
            event: params["event"].clone(),
        },
        "approval.request" => Notice::Approval {
            branch: params["branchId"].as_str().unwrap_or("main").to_owned(),
            request: params["requestId"].as_str().unwrap_or_default().to_owned(),
        },
        _ => Notice::Other,
    }
}

/// The quiet check's state for every known line: the one `session.idle`
/// request the line may have outstanding, and the highest event sequence the
/// driver has read from its notices.
#[derive(Default)]
pub(crate) struct IdleCalls {
    lines: BTreeMap<String, IdleCall>,
}

/// One line's outstanding idle request and the latest event read on it.
#[derive(Default)]
struct IdleCall {
    /// The line's `session.idle` request, kept until its reply is read: a
    /// sent request is never dropped, so a line whose request is unanswered
    /// is busy and the next check waits on the same one.
    request: Option<u64>,
    /// The highest event sequence the driver has read from the line.
    read: u64,
}

impl IdleCalls {
    /// The line's outstanding request, or `None` when it has none and the
    /// caller sends one, recording it with [`Self::sent`].
    pub(crate) fn outstanding(&self, branch: &str) -> Option<u64> {
        self.lines.get(branch).and_then(|line| line.request)
    }
    /// Records the request sent for the line.
    pub(crate) fn sent(&mut self, branch: &str, id: u64) {
        self.lines.entry(branch.to_owned()).or_default().request = Some(id);
    }
    /// Records the event `sequence` read from the line's notices.
    pub(crate) fn read(&mut self, branch: &str, sequence: u64) {
        let line = self.lines.entry(branch.to_owned()).or_default();
        line.read = line.read.max(sequence);
    }
    /// Records the line's reply and takes its request. `Ok(true)` counts the
    /// line as idle: the reply is at or after every event read on the line,
    /// or the line has ended. `Ok(false)` is a stale reply — one below an
    /// event the driver has read — so the request is spent and the line is
    /// busy. `Err` fails the run with the reply's text.
    pub(crate) fn reply(
        &mut self,
        branch: &str,
        result: dim_client::Result<Value>,
    ) -> std::result::Result<bool, String> {
        let line = self.lines.entry(branch.to_owned()).or_default();
        line.request = None;
        match result {
            Ok(value) => value
                .get("sequence")
                .and_then(Value::as_u64)
                .ok_or_else(|| "session.idle replied with no sequence".to_owned())
                .map(|sequence| sequence >= line.read),
            // An ended line has no turn to wait for.
            Err(error) if dim_client::app_error(&error) == Some("branch_ended") => Ok(true),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Whether every known line is idle and no notice arrived while it asked.
/// `session.idle` returns when the line is idle; a short deadline keeps a busy
/// line from holding the driver, and a call whose deadline passes stays
/// outstanding, so the connection holds at most one idle request per line. A
/// notice that arrives while a call waits is left in the client's queue, and
/// the driver reads every queued notice before its next quiet check, so
/// `false` here means the quiet period follows that notice's read.
async fn idle(
    client: &mut Client,
    session: &str,
    branches: &BTreeSet<String>,
    calls: &mut IdleCalls,
) -> std::result::Result<bool, String> {
    for branch in branches {
        let request = match calls.outstanding(branch) {
            Some(request) => request,
            None => {
                let request = client
                    .send(
                        "session.idle",
                        json!({"sessionId": session, "branchId": branch}),
                    )
                    .await
                    .map_err(|error| format!("{branch}: session.idle failed: {error}"))?;
                calls.sent(branch, request);
                request
            }
        };
        match tokio::time::timeout(IDLE_WAIT, client.response(request)).await {
            // The line has not gone idle: the request stays outstanding and
            // the next check waits on the same one.
            Err(_) => return Ok(false),
            Ok(reply) => match calls.reply(branch, reply) {
                Ok(true) => {}
                // The reply is below an event already read on the line.
                Ok(false) => return Ok(false),
                Err(error) => return Err(format!("{branch}: {error}")),
            },
        }
    }
    Ok(!client
        .notices
        .iter()
        .any(|message| matches!(message, WireMessage::Notice { .. })))
}

/// The export as the judge reads it (issue #635): every line in order, with
/// each request-bearing event's request rendered against the previous request
/// of the same line, so a run does not carry a request's unchanged history
/// once per model call.
fn judge_view(export: &str) -> String {
    let mut view = String::new();
    let mut previous: BTreeMap<String, Value> = BTreeMap::new();
    for raw in export.lines() {
        let Ok(mut line) = serde_json::from_str::<Value>(raw) else {
            view.push_str(raw);
            view.push('\n');
            continue;
        };
        if line["event"]["data"]["request"].is_object() {
            let branch = line["branch_id"].as_str().unwrap_or_default().to_owned();
            let request = line["event"]["data"]["request"].clone();
            line["event"]["data"]["request"] = rendered_request(&request, previous.get(&branch));
            previous.insert(branch, request);
        }
        view.push_str(&serde_json::to_string(&line).expect("a parsed line serializes"));
        view.push('\n');
    }
    view
}

/// One request as [`judge_view`] renders it, starting from the request object
/// itself so that nothing is dropped: the messages added since the line's
/// previous request, when that request's messages are a prefix of this one's,
/// or all of them otherwise; every other field in full when it differs from
/// the previous request's, or a marker when it does not. Every request can be
/// rebuilt from the view, whatever fields the request type gains later.
fn rendered_request(request: &Value, previous: Option<&Value>) -> Value {
    let messages = request["messages"].as_array();
    let added = messages.and_then(|messages| {
        let previous = previous?["messages"].as_array()?;
        let added = messages.get(previous.len()..)?;
        messages[..previous.len()]
            .iter()
            .zip(previous)
            .all(|(message, earlier)| message == earlier)
            .then_some(added)
    });
    let mut rendered = serde_json::Map::new();
    for (field, value) in request.as_object().into_iter().flatten() {
        if field == "messages" {
            match added {
                Some(added) => rendered.insert("addedMessages".to_owned(), json!(added)),
                None => rendered.insert("messages".to_owned(), request["messages"].clone()),
            };
            rendered.insert(
                "messageCount".to_owned(),
                json!(messages.map_or(0, Vec::len)),
            );
            continue;
        }
        let unchanged = previous.is_some_and(|previous| previous.get(field) == Some(value));
        rendered.insert(
            field.clone(),
            if unchanged {
                json!({"unchanged": true})
            } else {
                value.clone()
            },
        );
    }
    Value::Object(rendered)
}

/// The judge's limits (model-prompts §8.1, #522): its context window and its
/// output limit, read from its selection snapshot or from the model
/// configuration. A judge that declares neither fails the evaluation before
/// its first call.
#[derive(Clone, Copy, Debug)]
struct JudgeWindow {
    window: u64,
    max_output: u64,
}
/// The judge's declared limits, read beside its selection: a product
/// profile's selection carries a capability snapshot; a development model
/// configuration declares its limits in the file and its selections carry
/// none, so the profile answers through its own context-model resolver, the
/// one a Session asks for its window.
async fn judge_window(
    judge: &ModelSelection,
    model_config: Option<&Path>,
    cancel: &CancellationToken,
) -> Result<JudgeWindow> {
    if let Some(capabilities) = judge
        .capability_snapshot
        .as_ref()
        .map(|snapshot| &snapshot.capabilities)
        .filter(|capabilities| capabilities.context_window > 0)
    {
        return Ok(JudgeWindow {
            window: capabilities.context_window,
            max_output: capabilities.max_output_tokens,
        });
    }
    if let Some(path) = model_config {
        let (models, _) = dim_app_server::cli::model_profile(path)
            .map_err(|error| format!("--model-config {}: {error}", path.display()))?;
        let request = ModelRequest::new(&judge.provider_id, &judge.model_id, vec![]);
        if let Ok(facts) = models.resolve(Arc::new(request), cancel.clone()).await {
            return Ok(JudgeWindow {
                window: facts.context_window() as u64,
                max_output: facts.output_tokens() as u64,
            });
        }
    }
    Err(format!(
        "the judge {}/{} declares no context window in its selection or the model configuration",
        judge.provider_id, judge.model_id
    )
    .into())
}

/// The judge's answer for one run: a record per probe, the judge's error,
/// whether the judge had to drop its temperature, and how many times it was
/// asked again. A view that does not fit the judge's budget — the window less
/// the reply's `maxTokens`, estimated with the product's counter — leaves the
/// run unjudged before any call. A reply that gives no verdict, or leaves a
/// probe without one, is asked again with what was wrong, at most [`REASKS`]
/// times; a call that fails during a re-ask ends the asking. An answer that
/// stays unusable leaves every probe `unparsed` (model-prompts §8.1).
async fn judge_export(
    host: &Arc<ModelHost>,
    resolver: &Arc<ProviderConnectionResolver>,
    judge: &ModelSelection,
    scenario: &Scenario,
    view: &str,
    limits: JudgeWindow,
    cancel: &CancellationToken,
) -> Judging {
    let statements = scenario
        .probes
        .iter()
        .map(|probe| format!("- id: {}, statement: {}", probe.id, probe.text))
        .collect::<Vec<_>>()
        .join("\n");
    let text =
        format!("<statements>\n{statements}\n</statements>\n\n<timeline>\n{view}\n</timeline>");
    let mut messages = vec![Message::text(Role::User, text)];
    let mut config = judge.config.clone().unwrap_or_default();
    config.temperature = Some(0.);
    let mut dropped = false;
    let mut reasks = 0;
    let mut calls: Vec<JudgeCall> = Vec::new();
    let whole = judge_request(judge, &messages, &config);
    let allowance = whole
        .config
        .as_ref()
        .and_then(|config| config.max_tokens)
        .unwrap_or_else(|| output_allowance(limits.window, limits.max_output) as f64);
    let facts = window_facts(limits.window, allowance, &whole);
    let estimate = facts.count_tokens(&whole);
    let budget = facts.context_window() - facts.output_tokens();
    if estimate > budget {
        return Judging {
            outcome: JudgeOutcome::Unfit,
            probes: unparsed(scenario),
            error: Some(format!(
                "the judge view needs about {estimate:.0} tokens; the judge's budget is {budget:.0}"
            )),
            reasks: 0,
            calls: Vec::new(),
            temperature_dropped: false,
        };
    }
    let (outcome, probes, error) = loop {
        let request = Arc::new(judge_request(judge, &messages, &config));
        let started = Instant::now();
        let reply: Completion = match complete_detailed(host, resolver, &request, cancel).await {
            Ok(reply) => {
                calls.push(JudgeCall::answered(&reply, started.elapsed()));
                reply
            }
            // A call that fails during a re-ask ends the asking, and the
            // error names the call that failed.
            Err(error) if reasks > 0 => {
                calls.push(JudgeCall::failed(started.elapsed()));
                break (
                    JudgeOutcome::CallFailed,
                    unparsed(scenario),
                    Some(format!("the judge failed on a re-ask: {error}")),
                );
            }
            Err(error) => {
                calls.push(JudgeCall::failed(started.elapsed()));
                // The first call is retried once without a temperature.
                config.temperature = None;
                dropped = true;
                let retry = Arc::new(judge_request(judge, &messages, &config));
                let started = Instant::now();
                match complete_detailed(host, resolver, &retry, cancel).await {
                    Ok(reply) => {
                        calls.push(JudgeCall::answered(&reply, started.elapsed()));
                        reply
                    }
                    Err(retry) => {
                        calls.push(JudgeCall::failed(started.elapsed()));
                        break (
                            JudgeOutcome::CallFailed,
                            unparsed(scenario),
                            Some(format!("the judge failed: {error}, then {retry}")),
                        );
                    }
                }
            }
        };
        // A call cut off at its cap is a failed call, not an unreadable
        // answer: a re-ask exists for an answer that does not read, not for a
        // call that was cut off.
        if reply.stop == ModelStopReason::MaxTokens {
            let cap = match reply.completion_tokens {
                Some(tokens) => {
                    format!("the judge stopped at its output cap ({tokens:.0} completion tokens)")
                }
                None => "the judge stopped at its output cap".to_owned(),
            };
            break (JudgeOutcome::CallFailed, unparsed(scenario), Some(cap));
        }
        let what = match verdicts(scenario, &reply.text) {
            Ok(probes) => break (JudgeOutcome::Judged, probes, None),
            Err(what) if reasks == REASKS => {
                break (
                    JudgeOutcome::Unreadable,
                    unparsed(scenario),
                    Some(format!(
                        "the judge answered with no usable verdicts: {what}\n{}",
                        reply.text
                    )),
                );
            }
            Err(what) => what,
        };
        reasks += 1;
        messages.push(Message::text(Role::Assistant, reply.text));
        messages.push(Message::text(
            Role::User,
            JUDGE_REASK.replace("{what}", &what),
        ));
    };
    Judging {
        outcome,
        probes,
        error,
        reasks,
        calls,
        temperature_dropped: dropped,
    }
}

/// The request one judge call carries: the conversation so far, the judge's
/// configuration and the fixed system prompt.
fn judge_request(
    judge: &ModelSelection,
    messages: &[Message],
    config: &ModelConfig,
) -> ModelRequest {
    let mut request = ModelRequest::new(&judge.provider_id, &judge.model_id, messages.to_vec());
    request.capability_snapshot = judge.capability_snapshot.clone();
    request.config = Some(config.clone());
    request.system = Some(Arc::new(SystemPrompt::Text(JUDGE_SYSTEM.into())));
    request
}

/// The reply's verdict for every probe, or why it cannot be used: it does not
/// read, or it answers no verdict for some probe. Ids the scenario does not
/// have are ignored.
fn verdicts(scenario: &Scenario, reply: &str) -> std::result::Result<Vec<ProbeRecord>, String> {
    let judged: BTreeMap<String, (Verdict, Option<String>)> = parse_judgements(reply)
        .map_err(|error| format!("it is not a JSON array of verdict objects ({error})"))?
        .into_iter()
        .map(|Judgement { id, verdict, quote }| (id, (verdict, quote.filter(|q| !q.is_empty()))))
        .collect();
    let missing = scenario
        .probes
        .iter()
        .filter(|probe| !judged.contains_key(&probe.id))
        .map(|probe| probe.id.as_str())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!("it has no verdict for {}", missing.join(", ")));
    }
    Ok(scenario
        .probes
        .iter()
        .map(|probe| {
            let (verdict, quote) = judged
                .get(&probe.id)
                .expect("every probe has a verdict: a missing one was rejected above");
            let verdict = match verdict {
                Verdict::Present => ProbeVerdict::Present,
                Verdict::Absent => ProbeVerdict::Absent,
                Verdict::Contradicted => ProbeVerdict::Contradicted,
            };
            ProbeRecord {
                id: probe.id.clone(),
                kind: probe.kind.name(),
                verdict,
                quote: quote.clone(),
            }
        })
        .collect())
}

/// One probe's counted runs in one arm, and the passes among them: a probe's
/// rate counts judged, non-failed runs only (model-prompts §8.1).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ProbeCounts {
    passed: u32,
    runs: u32,
}

/// The arm's metrics over its scenarios and runs (model-prompts §8.1). An
/// unjudged or failed run adds nothing to the probe rates; the arm counts it
/// in its own rate.
#[derive(Default)]
struct Metrics {
    /// `(scenario, probe)` → its counted runs and the passes among them.
    probes: BTreeMap<(String, String), ProbeCounts>,
    /// Every `must_show` probe's counted runs and the passes among them.
    must_show: Counts,
    /// Every `must_not_show` probe's counted runs and the violations among
    /// them.
    must_not_show: Counts,
    failed: u32,
    /// Runs the judge gave no answer to, that did not fail on their own,
    /// left out of every other count.
    unjudged: u32,
    /// Every run of the arm, failed and unjudged included.
    runs: u32,
}
impl Metrics {
    fn add(&mut self, scenario: &Scenario, record: &RunRecord) {
        self.runs += 1;
        if record.unjudged() {
            self.unjudged += 1;
            return;
        }
        if record.failed {
            self.failed += 1;
            return;
        }
        for probe in &scenario.probes {
            let verdict = record
                .probes
                .iter()
                .find(|record| record.id == probe.id)
                .map(|record| record.verdict);
            let passed = match probe.kind {
                ProbeKind::MustShow => verdict == Some(ProbeVerdict::Present),
                ProbeKind::MustNotShow => {
                    matches!(
                        verdict,
                        Some(ProbeVerdict::Absent | ProbeVerdict::Contradicted)
                    )
                }
            };
            let counts = self
                .probes
                .entry((scenario.id.clone(), probe.id.clone()))
                .or_default();
            counts.runs += 1;
            counts.passed += u32::from(passed);
            match probe.kind {
                ProbeKind::MustShow => {
                    self.must_show.outcome += u64::from(passed);
                    self.must_show.total += 1;
                }
                ProbeKind::MustNotShow => {
                    self.must_not_show.outcome += u64::from(!passed);
                    self.must_not_show.total += 1;
                }
            }
        }
    }
    /// The probe's counted runs, or none when the arm ran none of them.
    fn counts(&self, key: &(String, String)) -> ProbeCounts {
        self.probes.get(key).copied().unwrap_or_default()
    }
}

/// The evaluation's family, fixed by the scenarios and their probes before
/// any run (model-prompts §8.1): every (scenario, probe) — a pass rate for a
/// `must_show` probe, a violation rate for a `must_not_show` one —, the
/// pooled `must_show` pass rate, the pooled `must_not_show` violation rate,
/// the failed-run rate and the unjudged-run rate. A probe kind no scenario
/// carries stays out of the family: its pooled row would have no data to
/// compare.
fn comparisons(current: &Metrics, candidate: &Metrics, scenarios: &[Scenario]) -> Vec<Comparison> {
    let mut comparisons = Vec::new();
    for scenario in scenarios {
        for probe in &scenario.probes {
            let key = (scenario.id.clone(), probe.id.clone());
            let (current_counts, candidate_counts) = (current.counts(&key), candidate.counts(&key));
            let counted = |counts: ProbeCounts| match probe.kind {
                ProbeKind::MustShow => Counts::new(counts.passed.into(), counts.runs.into()),
                ProbeKind::MustNotShow => {
                    Counts::new((counts.runs - counts.passed).into(), counts.runs.into())
                }
            };
            comparisons.push(Comparison {
                name: format!("{}/{} ({})", scenario.id, probe.id, probe.kind.name()),
                worse: match probe.kind {
                    ProbeKind::MustShow => Worse::Lower,
                    ProbeKind::MustNotShow => Worse::Higher,
                },
                current: counted(current_counts),
                candidate: counted(candidate_counts),
            });
        }
    }
    let rate = |counts: u32, runs: u32| Counts::new(counts.into(), runs.into());
    let carries = |kind: ProbeKind| {
        scenarios
            .iter()
            .any(|scenario| scenario.probes.iter().any(|probe| probe.kind == kind))
    };
    if carries(ProbeKind::MustShow) {
        comparisons.push(Comparison {
            name: "pooled must_show".into(),
            worse: Worse::Lower,
            current: current.must_show,
            candidate: candidate.must_show,
        });
    }
    if carries(ProbeKind::MustNotShow) {
        comparisons.push(Comparison {
            name: "pooled must_not_show (violations)".into(),
            worse: Worse::Higher,
            current: current.must_not_show,
            candidate: candidate.must_not_show,
        });
    }
    comparisons.push(Comparison {
        name: "failed runs".into(),
        worse: Worse::Higher,
        current: rate(current.failed, current.runs),
        candidate: rate(candidate.failed, candidate.runs),
    });
    comparisons.push(Comparison {
        name: "unjudged runs".into(),
        worse: Worse::Higher,
        current: rate(current.unjudged, current.runs),
        candidate: rate(candidate.unjudged, candidate.runs),
    });
    comparisons
}

/// The finished evaluations a report was built from, in the order given: each
/// directory with the runs it supplied. Such a report names them, and leaves
/// out what no saved run records.
#[derive(Clone)]
struct Sources {
    directories: Vec<(PathBuf, usize)>,
    /// Whether the model and judge rows are the flags rather than the judge
    /// that made these records: a pool judges nothing.
    given: bool,
}
impl Sources {
    /// The row that names the sources and the runs each supplied.
    fn row(&self) -> String {
        let mut row = self
            .directories
            .iter()
            .map(|(directory, runs)| format!("`{}` ({runs} runs)", directory.display()))
            .collect::<Vec<_>>()
            .join(", ");
        if self.given {
            row += concat!(
                "; the model and judge rows are those given to --pool; each source's ",
                "scenario.md states the instrument its runs were judged with",
            );
        }
        row
    }
}

/// The instrument of an evaluation: the texts under test with each
/// candidate's own token cost, the model and the judge with the configuration
/// they carry, and the runs (model-prompts §8.1). A run resolves the
/// configurations and the judge's window, so every run measures its own copy;
/// the evaluation merges them in schedule order and the report reads the
/// first.
#[derive(Clone)]
struct Instrument {
    texts: Vec<String>,
    model: ModelId,
    judge: ModelId,
    runs: usize,
    tokens: Vec<(String, f64)>,
    model_configuration: Option<Configuration>,
    judge_configuration: Option<Configuration>,
    /// The judge's declared limits, read from a run's selection.
    judge_window: Option<JudgeWindow>,
    /// The finished evaluations the report was built from instead of runs.
    sources: Option<Sources>,
    judge_temperature_dropped: usize,
    /// Runs the judge was asked about whose record does not say whether it
    /// dropped the temperature: a record from before the runner wrote it.
    judge_temperature_unrecorded: usize,
    /// Runs the judge was asked about: every outcome but `not_asked` and a
    /// view that does not fit its budget.
    judge_asked_runs: usize,
    /// How many re-asks the judge was asked for over every run.
    judge_reasks: usize,
    /// How many declines the runner sent again over every run.
    decline_retries: usize,
    /// Runs whose record does not say how many declines the runner sent
    /// again: a record from before the runner wrote it.
    decline_retries_unrecorded: usize,
}
impl Instrument {
    /// The instrument before any run: the texts under test with each
    /// candidate's own token cost, the model and the judge. A run adds the
    /// configurations and the window it resolved.
    fn new(options: &Options) -> Self {
        Self {
            texts: options.texts.iter().map(|(text, _)| text.clone()).collect(),
            model: options.model.clone(),
            judge: options.judge.clone(),
            runs: options.runs,
            tokens: options
                .texts
                .iter()
                .map(|(text, candidate)| (text.clone(), text_tokens(&options.model, candidate)))
                .collect(),
            model_configuration: None,
            judge_configuration: None,
            judge_window: None,
            sources: None,
            judge_temperature_dropped: 0,
            judge_temperature_unrecorded: 0,
            judge_asked_runs: 0,
            judge_reasks: 0,
            decline_retries: 0,
            decline_retries_unrecorded: 0,
        }
    }
    /// Adds one run's measurement. The configurations and the window are the
    /// same in every run, so the first one kept stands; the counts add up.
    fn merge(&mut self, record: &RunRecord, measured: Instrument) {
        self.model_configuration = self
            .model_configuration
            .take()
            .or(measured.model_configuration);
        self.judge_configuration = self
            .judge_configuration
            .take()
            .or(measured.judge_configuration);
        self.judge_window = self.judge_window.take().or(measured.judge_window);
        let asked = !matches!(
            record.judge_outcome,
            JudgeOutcome::NotAsked | JudgeOutcome::Unfit
        );
        match record.temperature_dropped {
            Some(dropped) => self.judge_temperature_dropped += usize::from(dropped),
            None if asked => self.judge_temperature_unrecorded += 1,
            None => {}
        }
        self.judge_asked_runs += usize::from(asked);
        self.judge_reasks += record.judge_reasks as usize;
        match record.decline_retries {
            Some(retries) => self.decline_retries += retries as usize,
            None => self.decline_retries_unrecorded += 1,
        }
    }
}

/// The reasoning effort and service tier a request carries (model-prompts
/// §8.1).
fn carried(configuration: &Option<Configuration>) -> String {
    let Some(configuration) = configuration else {
        return "no configuration".to_owned();
    };
    format!(
        "reasoning effort {}, service tier {}",
        configuration.effort.as_deref().unwrap_or("none"),
        configuration.tier.as_deref().unwrap_or("none"),
    )
}

/// Whether two model ids name one model family: the same provider and the
/// same family word, the last `/`-separated segment's first `-`-separated
/// word (model-prompts §8.1).
fn same_family(model: &ModelId, judge: &ModelId) -> bool {
    model.provider_id == judge.provider_id && family(&model.model_id) == family(&judge.model_id)
}
fn family(model_id: &str) -> &str {
    model_id
        .rsplit('/')
        .next()
        .unwrap_or(model_id)
        .split('-')
        .next()
        .unwrap_or(model_id)
}

fn report(instrument: &Instrument, scenarios: &[Scenario], records: &[RunRecord]) -> String {
    let mut metrics: BTreeMap<&'static str, Metrics> = BTreeMap::new();
    for record in records {
        let Some(scenario) = scenarios
            .iter()
            .find(|scenario| scenario.id == record.scenario)
        else {
            continue;
        };
        metrics.entry(record.arm).or_default().add(scenario, record);
    }
    let window = instrument.judge_window.map_or_else(String::new, |window| {
        format!(
            ", window {}, max output {}",
            window.window, window.max_output
        )
    });
    let mut text = String::from("| | |\n| --- | --- |\n");
    // A report built from saved runs names its sources instead: no saved run
    // records the texts under test.
    if instrument.sources.is_none() {
        let texts: Vec<String> = instrument
            .texts
            .iter()
            .map(|text| format!("`{text}`"))
            .collect();
        text += &format!("| text | {} |\n", texts.join(", "));
    }
    text += &format!(
        "| model | `{}/{}`, {} |\n| judge | `{}/{}`, temperature 0, {}{window} |\n| runs | {} per arm and scenario |\n",
        instrument.model.provider_id,
        instrument.model.model_id,
        carried(&instrument.model_configuration),
        instrument.judge.provider_id,
        instrument.judge.model_id,
        carried(&instrument.judge_configuration),
        instrument.runs,
    );
    if instrument.sources.is_none() {
        let tokens: Vec<String> = instrument
            .tokens
            .iter()
            .map(|(text, tokens)| format!("`{text}`: {tokens:.0}"))
            .collect();
        text += &format!("| candidate tokens | {} |\n", tokens.join("; "));
    }
    text += &format!(
        "| rule | a comparison is a regression when the candidate's loss exceeds {delta:.2} and a one-sided mid-p Fisher exact test on both arms' counts rejects it, Holm-adjusted at a family-wise α = {alpha:.2}; the verdict needs {floor} judged runs per arm and scenario |\n",
        delta = rule::DELTA,
        alpha = rule::ALPHA,
        floor = rule::FLOOR,
    );
    if let Some(sources) = &instrument.sources {
        text += &format!("| sources | {} |\n", sources.row());
    }
    if instrument.decline_retries > 0 || instrument.decline_retries_unrecorded > 0 {
        let mut row = format!(
            "| decline retries | {} across all runs",
            instrument.decline_retries
        );
        if instrument.decline_retries_unrecorded > 0 {
            row += &format!(
                "; not recorded for {} runs",
                instrument.decline_retries_unrecorded
            );
        }
        text += &format!("{row} |\n");
    }
    if same_family(&instrument.model, &instrument.judge) {
        text += "| judge limitation | the judge is the model's own family; the measurement is limited by that |\n";
    }
    if instrument.judge_temperature_dropped > 0 || instrument.judge_temperature_unrecorded > 0 {
        let mut row = format!(
            "| judge temperature | dropped in {} of {} runs the judge was asked",
            instrument.judge_temperature_dropped, instrument.judge_asked_runs
        );
        if instrument.judge_temperature_unrecorded > 0 {
            row += &format!(
                "; not recorded for {} runs",
                instrument.judge_temperature_unrecorded
            );
        }
        text += &format!("{row} |\n");
    }
    if instrument.judge_reasks > 0 {
        text += &format!(
            "| judge re-asks | {} across all runs |\n",
            instrument.judge_reasks
        );
    }
    text.push('\n');
    for scenario in scenarios {
        text += &format!(
            "- `{}`{}: {} turns, {} seconds, window {}",
            scenario.id,
            if scenario.description.is_empty() {
                String::new()
            } else {
                format!(": {}", scenario.description)
            },
            scenario.budget.turns,
            scenario.budget.seconds,
            scenario.budget.window
        );
        text.push('\n');
    }
    let empty = Metrics::default();
    let current = metrics.get("current").unwrap_or(&empty);
    let candidate = metrics.get("candidate").unwrap_or(&empty);
    // The fewest judged runs an arm ran a scenario: the floor the verdict
    // needs. Failed and unjudged runs are compared in their own rows and do
    // not count here, because they enter no probe rate (model-prompts §8.1).
    let mut per_arm: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for record in records.iter().filter(|record| record.judged()) {
        *per_arm
            .entry((record.arm, record.scenario.as_str()))
            .or_default() += 1;
    }
    let mut min_runs = u64::MAX;
    for arm in ["current", "candidate"] {
        for scenario in scenarios {
            let runs = per_arm
                .get(&(arm, scenario.id.as_str()))
                .copied()
                .unwrap_or(0);
            min_runs = min_runs.min(runs);
        }
    }
    if scenarios.is_empty() {
        min_runs = 0;
    }
    let family = comparisons(current, candidate, scenarios);
    let judged = rule::judge(&family, min_runs);
    text.push('\n');
    text.push_str(rule::COMPARISON_HEADER);
    for row in &judged.rows {
        text += &row.line();
        text.push('\n');
    }
    let unjudged: Vec<&RunRecord> = records.iter().filter(|record| record.unjudged()).collect();
    if !unjudged.is_empty() {
        text += "\n| arm | scenario | run | judge error |\n| --- | --- | --- | --- |\n";
        for record in unjudged {
            let error = record.judge_error.as_deref().unwrap_or_default();
            text += &format!(
                "| {} | {} | {} | {} |\n",
                record.arm,
                record.scenario,
                record.run,
                error.lines().next().unwrap_or_default(),
            );
        }
    }
    text.push('\n');
    text.push_str(&rule::verdict_line(
        &judged,
        min_runs,
        "scenario",
        "candidate",
    ));
    text.push('\n');
    text
}

/// One run's private endpoint directory, short enough for Unix socket addresses.
/// Dropping it removes only that run's endpoints.
pub(crate) struct Sockets(pub(crate) PathBuf);
impl Sockets {
    pub(crate) fn new() -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        private_directory(&directory)?;
        Ok(Self(directory))
    }
}
impl Drop for Sockets {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates `path` private to the user: the App Server serves its local
/// endpoint only from a 0700 directory.
pub(crate) fn private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Copies a directory tree. Only files and directories are copied; anything
/// else in a scenario's workspace is an error naming the path.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        } else {
            return Err(format!(
                "{}: only files and directories are copied",
                entry.path().display()
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "eval-tests"))]
mod tests {
    use super::*;
    use dim_agent_capabilities::host::{
        HostFuture, ModelConnection, ModelDriver, ModelStopReason, ResolveModelConnection,
        ResolvedModelConnection,
    };
    use dim_agent_capabilities::host_platform::{HostPlatform, resolve_shell};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    fn probe(id: &str, kind: ProbeKind) -> Probe {
        Probe {
            id: id.into(),
            kind,
            text: format!("statement {id}"),
        }
    }
    fn scenario(probes: Vec<Probe>) -> Scenario {
        Scenario {
            id: "case".into(),
            description: String::new(),
            directory: PathBuf::new(),
            message: "message".into(),
            calls: Vec::new(),
            sources: Vec::new(),
            events: Vec::new(),
            steers: Vec::new(),
            budget: Budget {
                turns: 4,
                seconds: 60.,
                window: 8_000,
            },
            probes,
        }
    }
    fn record(
        arm: &'static str,
        scenario: &Scenario,
        verdicts: &[(&str, ProbeVerdict)],
        judge_outcome: JudgeOutcome,
    ) -> RunRecord {
        RunRecord {
            arm,
            scenario: scenario.id.clone(),
            run: 0,
            session: None,
            stop: "idle",
            turns: 1,
            window: None,
            duration_ms: 0.,
            failed: failed("idle", None, judge_outcome),
            judge_outcome,
            error: None,
            judge_error: None,
            judge_reasks: 0,
            judge_calls: Vec::new(),
            temperature_dropped: Some(false),
            decline_retries: Some(0),
            events: Vec::new(),
            reply_calls: Vec::new(),
            probes: verdicts
                .iter()
                .map(|(id, verdict)| ProbeRecord {
                    id: (*id).into(),
                    kind: scenario
                        .probes
                        .iter()
                        .find(|probe| probe.id == *id)
                        .map_or("must_show", |probe| probe.kind.name()),
                    verdict: *verdict,
                    quote: None,
                })
                .collect(),
        }
    }
    fn metrics(scenario: &Scenario, records: &[RunRecord]) -> Metrics {
        let mut metrics = Metrics::default();
        for record in records {
            metrics.add(scenario, record);
        }
        metrics
    }
    fn model() -> ModelId {
        ModelId {
            provider_id: "gateway".into(),
            model_id: "deepseek-v4.1-flash".into(),
        }
    }
    /// One evaluation's instrument, reading `judge`'s verdicts.
    fn instrument(judge: ModelId) -> Instrument {
        Instrument {
            texts: vec!["context-compaction/summary".into()],
            model: model(),
            judge,
            runs: 1,
            tokens: vec![("context-compaction/summary".into(), 1.)],
            model_configuration: None,
            judge_configuration: None,
            judge_window: None,
            sources: None,
            judge_temperature_dropped: 0,
            judge_temperature_unrecorded: 0,
            judge_asked_runs: 0,
            judge_reasks: 0,
            decline_retries: 0,
            decline_retries_unrecorded: 0,
        }
    }

    /// The `--text`/`--candidate` pairs a test hands the runner.
    fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, Arc<str>)> {
        pairs
            .iter()
            .map(|(text, candidate)| ((*text).to_owned(), Arc::from(*candidate)))
            .collect()
    }

    /// An unknown `--text` id fails before any run and lists the known ids.
    #[test]
    fn an_unknown_text_id_fails_before_any_run() {
        let error = overrides(&pairs(&[("no-such-text", "candidate")]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no-such-text"), "{error}");
        assert!(error.contains("context-compaction/summary"), "{error}");
        assert!(
            error.contains("timeline-tree/branch-description"),
            "{error}"
        );
        assert!(error.contains("timeline-tree/roster-closing"), "{error}");
        let known = overrides(&pairs(&[("context-compaction/summary", "candidate")])).unwrap();
        assert_eq!(known.summary_prompt.as_deref(), Some("candidate"));
        let branch =
            overrides(&pairs(&[("timeline-tree/branch-description", "candidate")])).unwrap();
        assert_eq!(branch.branch_description.as_deref(), Some("candidate"));
        let roster = overrides(&pairs(&[("timeline-tree/roster-closing", "candidate")])).unwrap();
        assert_eq!(roster.roster_closing.as_deref(), Some("candidate"));
    }

    /// Goal's two texts are members of its native crate's policy sentences,
    /// applied through the same injection (Goal A.5.1, model-prompts §8.1).
    #[test]
    fn the_goal_texts_are_native_policy_members() {
        let objective = overrides(&pairs(&[("goal/objective", "candidate")])).unwrap();
        assert_eq!(objective.goal_objective.as_deref(), Some("candidate"));
        assert_eq!(objective.goal_continuation, None);
        let both = overrides(&pairs(&[
            ("goal/objective", "first"),
            ("goal/continuation", "second"),
        ]))
        .unwrap();
        assert_eq!(both.goal_objective.as_deref(), Some("first"));
        assert_eq!(both.goal_continuation.as_deref(), Some("second"));
    }

    /// An event records its type's declared mode, whatever mode the
    /// scenario's own subscription names: a delivery's effective mode comes
    /// from the subscriptions the receipt lists (EventHub design §3.1, A.2).
    #[test]
    fn an_event_keeps_its_type_declared_mode() {
        fn event_mode(
            directory: &std::path::Path,
            subscribe: Value,
            declared: Value,
        ) -> (InputMode, bool) {
            let file: ScenarioFile = serde_json::from_value(json!({
                "message": "message",
                "sources": [{
                    "name": "chat",
                    "guide": "Messages from the team's chat.",
                    "types": [{
                        "type": "chat.message",
                        "description": "A message from the team's chat.",
                        "mode": declared
                    }],
                    "subscribe": {"types": ["chat.message"], "mode": subscribe}
                }],
                "events": [{
                    "source": "chat", "type": "chat.message", "id": "m-1",
                    "body": "question", "whenToolCall": "read"
                }],
                "budget": {"turns": 1, "seconds": 60, "window": 1000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }))
            .unwrap();
            let scenario = into_scenario("case".into(), directory.to_path_buf(), file).unwrap();
            let mode = scenario.events[0].declared_mode;
            (mode, starts_turn(mode))
        }
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-event-mode-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(directory.join("workspace")).unwrap();
        assert_eq!(
            event_mode(&directory, json!("append"), json!("queue")),
            (InputMode::Queue, true),
            "the scenario's subscription mode stays out of the event; the type's mode is kept"
        );
        assert_eq!(
            event_mode(&directory, Value::Null, json!("queue")),
            (InputMode::Queue, true),
            "without a subscription the type's declared mode applies"
        );
        assert_eq!(
            event_mode(&directory, json!("steer"), json!("append")),
            (InputMode::Append, false),
            "an append-typed event starts no turn"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A scenario's steers are the operator's: they go out in Steer mode, so
    /// a waiting `main` takes them, and the initial message carries no mode
    /// and is queued input (issue #728).
    #[test]
    fn a_steer_is_submitted_in_steer_mode_and_the_message_without_one() {
        let params = submit_params("session", "go", None);
        assert!(params.get("mode").is_none(), "{params}");
        let message: dim_app_server::protocol::Submit =
            serde_json::from_value(params).expect("the first message is a Submit");
        assert_eq!(message.text.as_deref(), Some("go"));
        assert_eq!(message.mode, None);
        let params = submit_params("session", "also", Some(InputMode::Steer));
        let steer: dim_app_server::protocol::Submit =
            serde_json::from_value(params).expect("a steer is a Submit");
        assert_eq!(steer.text.as_deref(), Some("also"));
        assert_eq!(steer.mode, Some(InputMode::Steer));
    }

    /// The committed set parses through the runner's own reader, and the two
    /// pairs that share a premise stay equal: the compact scenario reuses
    /// `redone-work`'s message and workspace, and `flooding` and
    /// `never-ending` share one workspace (Timeline branches §12.3). No model
    /// runs, so CI catches a schema, layout or pair drift before a manual run.
    #[test]
    fn the_committed_scenario_set_parses_and_its_shared_premises_stay_equal() {
        let root = repository().join("xtask").join("scenario-eval");
        let branches = read_scenarios(&root.join("timeline-branches")).unwrap();
        let compact = read_scenarios(&root.join("timeline-branches-compact")).unwrap();
        fn find<'a>(scenarios: &'a [Scenario], id: &str) -> &'a Scenario {
            scenarios
                .iter()
                .find(|scenario| scenario.id == id)
                .unwrap_or_else(|| panic!("{id} is in the committed set"))
        }
        let redone = find(&branches, "redone-work");
        let identity = find(&compact, "identity-after-compaction");
        assert_eq!(
            identity.message, redone.message,
            "identity-after-compaction runs redone-work's task"
        );
        assert_eq!(
            workspace_files(&identity.directory),
            workspace_files(&redone.directory),
            "identity-after-compaction starts from redone-work's workspace"
        );
        let flooding = find(&branches, "flooding");
        let never_ending = find(&branches, "never-ending");
        assert_eq!(
            workspace_files(&flooding.directory),
            workspace_files(&never_ending.directory),
            "flooding and never-ending start from one workspace"
        );
    }

    /// The scheduler set parses through the runner's own reader (Scheduler
    /// §5.1): three scenarios, each with a statement to see and a statement
    /// not to see, and no EventHub source, since the subject is the `schedule`
    /// tool. No model runs, so CI catches a schema or layout drift before a
    /// manual run.
    #[test]
    fn the_scheduler_scenario_set_parses() {
        let root = repository()
            .join("xtask")
            .join("scenario-eval")
            .join("scheduler");
        let scenarios = read_scenarios(&root).unwrap();
        let ids: Vec<_> = scenarios
            .iter()
            .map(|scenario| scenario.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "a-target-the-tool-cannot-set",
                "recurring-job-from-a-request",
                "rename-an-existing-job",
            ],
            "the committed set is the three scenarios"
        );
        for scenario in &scenarios {
            assert!(
                scenario.sources.is_empty() && scenario.events.is_empty(),
                "{}: the schedule tool is the subject, not an EventHub source",
                scenario.id
            );
            assert!(
                scenario
                    .probes
                    .iter()
                    .any(|probe| probe.kind == ProbeKind::MustShow),
                "{}: a statement to see",
                scenario.id
            );
            assert!(
                scenario
                    .probes
                    .iter()
                    .any(|probe| probe.kind == ProbeKind::MustNotShow),
                "{}: a statement not to see",
                scenario.id
            );
        }
    }

    /// The `schedule` description is injected through the product's own
    /// factory member (Scheduler §5.1, model-prompts §8.1): `--text
    /// scheduler/description` lands on the `Overrides` field
    /// [`app_server::serve_home`] hands `SchedulerTexts`, never on a
    /// product string patched for the evaluation.
    #[test]
    fn the_schedule_description_is_a_factory_member() {
        let overrides = overrides_in(
            TEXTS,
            &[(
                "scheduler/description".to_owned(),
                Arc::from("candidate description"),
            )],
        )
        .unwrap();
        assert_eq!(
            overrides.scheduler_description.as_deref(),
            Some("candidate description")
        );
        assert!(overrides.branch_description.is_none());
    }

    /// The `scheduler/description` candidate survives the serve child's JSON
    /// overrides and arrives in the run's model request as the `schedule`
    /// tool's declaration (§5.1): the candidate arm's every request names the
    /// tool with the candidate text, the current arm's with the shipped one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_schedule_description_candidate_reaches_the_run_through_the_child() {
        const CANDIDATE: &str = "CANDIDATE SCHEDULE TOOL DESCRIPTION";
        /// The shipped description's opening (`session.rs` `DESCRIPTION`).
        const SHIPPED: &str = "Manage scheduled jobs across this host";
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| Some(content("ok")))
        }
        let fixture = Fixture::start(
            "scheduler-description",
            answers,
            json!({
                "message": "Say ok.",
                "budget": {"turns": 2, "seconds": 60, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let candidate = overrides(&pairs(&[("scheduler/description", CANDIDATE)])).unwrap();
        for (arm, overrides) in [("current", Overrides::default()), ("candidate", candidate)] {
            let record = run_arm(&fixture, &options, arm, &overrides, 0).await;
            assert_eq!(record.error, None, "{arm}");
            assert!(!record.failed, "{arm}");
        }
        // The `schedule` tool's description in each request of an arm, one
        // entry per request that declares the tool.
        let descriptions = |arm: &str| -> Vec<Option<String>> {
            fixture
                .arm(arm)
                .iter()
                .filter_map(|request| serde_json::from_str::<Value>(request).ok())
                .filter_map(|request| {
                    request["tools"].as_array().and_then(|tools| {
                        tools
                            .iter()
                            .find(|tool| tool["function"]["name"] == "schedule")
                            .map(|tool| tool["function"]["description"].as_str().map(str::to_owned))
                    })
                })
                .collect()
        };
        let current = descriptions("current");
        let offered = descriptions("candidate");
        assert!(
            !current.is_empty(),
            "the current arm's request declares the schedule tool"
        );
        assert!(
            !offered.is_empty(),
            "the candidate arm's request declares the schedule tool"
        );
        assert!(
            current.iter().all(|description| description
                .as_deref()
                .is_some_and(|text| text.contains(SHIPPED))),
            "the current arm carries the shipped description: {current:?}"
        );
        assert!(
            offered
                .iter()
                .all(|description| description.as_deref() == Some(CANDIDATE)),
            "the candidate arm's schedule tool description is the candidate text: {offered:?}"
        );
    }

    /// Every file under a scenario's `workspace/`, by path under it.
    fn workspace_files(directory: &Path) -> BTreeMap<PathBuf, String> {
        fn walk(root: &Path, directory: &Path) -> BTreeMap<PathBuf, String> {
            let mut files = BTreeMap::new();
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    files.extend(walk(root, &path));
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_owned(),
                        String::from_utf8(std::fs::read(&path).unwrap())
                            .expect("a scenario workspace file is text"),
                    );
                }
            }
            files
        }
        walk(directory, &directory.join("workspace"))
    }

    /// A re-judge or a pool reads a finished evaluation's saved runs, so a
    /// flag that names a new evaluation, or a selection that read does not
    /// take, is refused rather than ignored.
    #[test]
    fn a_saved_run_read_refuses_the_flags_it_does_not_read() {
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-saved-parse-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let out = directory.join("out");
        let shared = [
            "--scenarios",
            "scenarios",
            "--model",
            "gateway/model",
            "--judge",
            "gateway/model",
            "--model-config",
            "models.json",
            "--out",
            out.to_str().unwrap(),
        ];
        let refused = |flags: &[&str]| {
            let args = shared.iter().chain(flags).map(|value| (*value).to_owned());
            match Options::parse_from(args) {
                Err(error) => error.to_string(),
                Ok(_) => panic!("{flags:?} is refused"),
            }
        };
        let error = refused(&["--rejudge", "source", "--runs", "1"]);
        assert!(error.contains("belong to a new evaluation"), "{error}");
        let error = refused(&[
            "--pool",
            "one",
            "--pool",
            "two",
            "--run",
            "current/case/run-0",
        ]);
        assert!(
            error.contains("--run selects a --rejudge source's runs"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// `--run` selects a run of a `--rejudge` source once: the same run
    /// selected twice — as `run-N` or as its index — is refused rather than
    /// reported twice.
    #[test]
    fn a_run_selected_twice_is_refused() {
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-run-twice-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let out = directory.join("out");
        for repeated in [
            ["current/case/run-0", "current/case/run-0"],
            ["current/case/run-0", "current/case/0"],
        ] {
            let args = [
                "--scenarios",
                "scenarios",
                "--model",
                "gateway/model",
                "--judge",
                "gateway/model",
                "--model-config",
                "models.json",
                "--out",
                out.to_str().unwrap(),
                "--rejudge",
                "source",
                "--run",
                repeated[0],
                "--run",
                repeated[1],
            ];
            let error = match Options::parse_from(args.map(str::to_owned)) {
                Err(error) => error.to_string(),
                Ok(_) => panic!("{repeated:?} is refused"),
            };
            assert!(
                error.contains(&format!(
                    "--run {} names a run already selected",
                    repeated[1]
                )),
                "{repeated:?}: {error}"
            );
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// `--run` selects a `--rejudge` source's runs: a new evaluation refuses
    /// it rather than running with the selection dropped.
    #[test]
    fn a_run_without_a_rejudge_source_is_refused() {
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-run-parse-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let candidate = directory.join("candidate.md");
        std::fs::write(&candidate, "candidate").unwrap();
        let out = directory.join("out");
        let args = [
            "--scenarios",
            "scenarios",
            "--text",
            "context-compaction/summary",
            "--candidate",
            candidate.to_str().unwrap(),
            "--model",
            "gateway/model",
            "--judge",
            "gateway/model",
            "--runs",
            "1",
            "--out",
            out.to_str().unwrap(),
            "--run",
            "current/case/run-0",
        ];
        let error = match Options::parse_from(args.map(str::to_owned)) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("--run without --rejudge is refused"),
        };
        assert!(
            error.contains("--run selects a --rejudge source's runs: give --rejudge DIR"),
            "{error}"
        );
        assert!(
            !out.exists(),
            "nothing is created before the flags are read"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Everything under a directory: its directories and their files, by
    /// relative path, with each file's bytes.
    fn tree(directory: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn walk(root: &Path, directory: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_owned();
                if path.is_dir() {
                    entries.insert(relative, None);
                    walk(root, &path, entries);
                } else {
                    entries.insert(relative, Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut entries = BTreeMap::new();
        walk(directory, directory, &mut entries);
        entries
    }

    /// A finished read never writes to its source: an `--out` inside it is
    /// refused however it is spelled, before `--out` is created, so the
    /// source is left byte for byte as it was, with no new directory or file.
    /// The spellings cover a plain nesting, the source itself, a missing
    /// component followed by `..`, and a link to the source.
    #[test]
    fn a_finished_read_refuses_an_out_inside_its_source() {
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-inside-source-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let source = directory.join("source");
        std::fs::create_dir_all(source.join("current").join("case").join("run-0")).unwrap();
        std::fs::write(source.join("runs.jsonl"), "{\"arm\": \"current\"}\n").unwrap();
        std::fs::write(source.join("scenario.md"), "the source's report").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&source, directory.join("link")).unwrap();
        }
        let before = tree(&directory);
        let outs = [
            vec![
                source.join("inside"),
                source.clone(),
                directory
                    .join("new")
                    .join("..")
                    .join("source")
                    .join("inside"),
            ],
            #[cfg(unix)]
            vec![
                directory.join("link").join("inside"),
                directory.join("new").join("..").join("link").join("inside"),
            ],
        ]
        .concat();
        for read in ["--rejudge", "--pool"] {
            for out in &outs {
                let args = [
                    "--scenarios",
                    "scenarios",
                    "--model",
                    "gateway/model",
                    "--judge",
                    "gateway/model",
                    "--model-config",
                    "models.json",
                    "--out",
                    out.to_str().unwrap(),
                    read,
                    source.to_str().unwrap(),
                ];
                let error = match Options::parse_from(args.map(str::to_owned)) {
                    Err(error) => error.to_string(),
                    Ok(_) => panic!(
                        "{read}: an --out inside the source is refused: {}",
                        out.display()
                    ),
                };
                assert!(
                    error.contains(&format!("--out {} is inside {read}", out.display())),
                    "{read}: {}: {error}",
                    out.display()
                );
                assert_eq!(
                    tree(&directory),
                    before,
                    "{read}: {}: nothing is created",
                    out.display()
                );
            }
            assert!(
                !directory.join("new").exists(),
                "{read}: the missing component is not created"
            );
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// `--out` is created where it resolves: a missing component followed by
    /// `..` carries the resolution back to the directory that exists, and is
    /// never created on the way.
    #[test]
    fn a_finished_read_creates_the_path_it_resolved() {
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-out-resolution-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let source = directory.join("source");
        std::fs::create_dir_all(&source).unwrap();
        let out = directory.join("new").join("..").join("elsewhere");
        let options = Options::parse_from(
            [
                "--scenarios",
                "scenarios",
                "--model",
                "gateway/model",
                "--judge",
                "gateway/model",
                "--model-config",
                "models.json",
                "--out",
                out.to_str().unwrap(),
                "--rejudge",
                source.to_str().unwrap(),
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            options.out,
            directory.canonicalize().unwrap().join("elsewhere"),
            "the parsed --out is the path the resolution reaches"
        );
        assert!(
            !directory.join("new").exists(),
            "the missing component is not created"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A link whose target does not exist is refused by name: the resolution
    /// cannot say where creating through it would land, so nothing is.
    #[cfg(unix)]
    #[test]
    fn a_finished_read_refuses_an_out_through_a_dangling_link() {
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-dangling-link-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let source = directory.join("source");
        std::fs::create_dir_all(&source).unwrap();
        let dangling = directory.join("dangling");
        std::os::unix::fs::symlink(directory.join("nowhere"), &dangling).unwrap();
        let entries = |directory: &Path| {
            let mut names: Vec<_> = std::fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before = entries(&directory);
        let out = dangling.join("x");
        for read in ["--rejudge", "--pool"] {
            let args = [
                "--scenarios",
                "scenarios",
                "--model",
                "gateway/model",
                "--judge",
                "gateway/model",
                "--model-config",
                "models.json",
                "--out",
                out.to_str().unwrap(),
                read,
                source.to_str().unwrap(),
            ];
            let error = match Options::parse_from(args.map(str::to_owned)) {
                Err(error) => error.to_string(),
                Ok(_) => panic!("{read}: an --out through a dangling link is refused"),
            };
            let link = directory.canonicalize().unwrap().join("dangling");
            assert!(
                error.contains(&format!("{} is a link", link.display())),
                "{read}: the error names the link: {error}"
            );
            assert_eq!(entries(&directory), before, "{read}: nothing is created");
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// `--text` repeats once per pair, and a text named twice is refused
    /// before any run (model-prompts §8.1).
    #[test]
    fn a_text_named_twice_is_refused() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-parse-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let candidate = directory.join("candidate.md");
        std::fs::write(&candidate, "candidate").unwrap();
        let parsed = Options::parse_from(
            [
                "--scenarios",
                "scenarios",
                "--text",
                "goal/objective",
                "--candidate",
                candidate.to_str().unwrap(),
                "--text",
                "goal/objective",
                "--candidate",
                candidate.to_str().unwrap(),
                "--model",
                "gateway/model",
                "--judge",
                "gateway/model",
                "--runs",
                "1",
                "--out",
                directory.join("out").to_str().unwrap(),
            ]
            .map(str::to_owned),
        );
        let error = match parsed {
            Err(error) => error.to_string(),
            Ok(_) => panic!("a text named twice is refused before any run"),
        };
        assert!(error.contains("goal/objective is named twice"), "{error}");
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A `--text ID --candidate FILE` pair reads in either order: the
    /// candidate given first pairs with the text that follows it
    /// (model-prompts §8.1).
    #[test]
    fn a_pair_reads_in_either_order() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-order-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let candidate = directory.join("candidate.md");
        std::fs::write(&candidate, "candidate").unwrap();
        let out = directory.join("out");
        let file = candidate.to_str().unwrap();
        let parse = |flags: [&str; 4]| {
            Options::parse_from(
                [
                    "--scenarios",
                    "scenarios",
                    flags[0],
                    flags[1],
                    flags[2],
                    flags[3],
                    "--model",
                    "gateway/model",
                    "--judge",
                    "gateway/model",
                    "--runs",
                    "1",
                    "--out",
                    out.to_str().unwrap(),
                ]
                .map(str::to_owned),
            )
        };
        for flags in [
            ["--text", "goal/objective", "--candidate", file],
            ["--candidate", file, "--text", "goal/objective"],
        ] {
            let options = parse(flags).unwrap_or_else(|error| panic!("{flags:?}: {error}"));
            assert_eq!(options.texts.len(), 1, "{flags:?}");
            assert_eq!(options.texts[0].0, "goal/objective", "{flags:?}");
            assert_eq!(&*options.texts[0].1, "candidate", "{flags:?}");
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The arms are interleaved: for each run index and scenario the current
    /// arm is scheduled before the candidate one (model-prompts §8.1). The
    /// order is fixed before any run starts, so `--jobs` cannot change which
    /// runs are made or in which order the records come out.
    #[test]
    fn the_schedule_alternates_the_arms() {
        assert_eq!(
            schedule(2, 2),
            vec![
                ("current", 0, 0),
                ("candidate", 0, 0),
                ("current", 1, 0),
                ("candidate", 1, 0),
                ("current", 0, 1),
                ("candidate", 0, 1),
                ("current", 1, 1),
                ("candidate", 1, 1),
            ]
        );
    }

    /// `--jobs 2` runs two runs at once, each with its own workspace copy and
    /// Session, and schedules the same runs as `--jobs 1`: the records are the
    /// four the schedule fixes, in its order (model-prompts §8.1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jobs_change_how_many_runs_are_in_flight_not_which() {
        let fixture = Fixture::start(
            "jobs",
            judged,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let mut options = fixture.options(fixture.directory.join("out"));
        options.runs = 2;
        options.jobs = 2;
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
        let records = fixture.records(&options.out);
        let ran: Vec<(&str, &str, u64)> = records
            .iter()
            .map(|record| {
                (
                    record["arm"].as_str().unwrap(),
                    record["scenario"].as_str().unwrap(),
                    record["run"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            ran,
            vec![
                ("current", "case", 0),
                ("candidate", "case", 0),
                ("current", "case", 1),
                ("candidate", "case", 1),
            ],
            "the same runs as --jobs 1, in the schedule's order"
        );
        for (arm, _, run) in &ran {
            assert!(
                options
                    .out
                    .join(arm)
                    .join("case")
                    .join(format!("run-{run}"))
                    .join("workspace")
                    .is_dir(),
                "{arm} run {run} kept its own workspace copy"
            );
        }
    }

    /// One arm's runs with `passed` of them passing a single `must_show`
    /// probe at run `index`.
    fn arm(scenario: &Scenario, arm: &'static str, passed: usize, runs: usize) -> Vec<RunRecord> {
        (0..runs)
            .map(|index| {
                let verdict = if index < passed {
                    ProbeVerdict::Present
                } else {
                    ProbeVerdict::Absent
                };
                record(arm, scenario, &[("reports", verdict)], JudgeOutcome::Judged)
            })
            .collect()
    }

    /// The runner judges the family through the rule module: identical counts
    /// pass at the floor of 20 runs, fall to inconclusive at 19, and a planted
    /// loss fails and names its comparison, below the floor too
    /// (model-prompts §8.1).
    #[test]
    fn the_runner_judges_the_family_at_the_floor() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let judged = |current: usize, candidate: usize, runs: usize| {
            let mut records = arm(&scenario, "current", current, runs);
            records.extend(arm(&scenario, "candidate", candidate, runs));
            report(
                &instrument(model()),
                std::slice::from_ref(&scenario),
                &records,
            )
        };
        let text = judged(20, 20, 20);
        assert!(
            text.contains(
                "| case/reports (must_show) | 20/20 (1.000) | 20/20 (1.000) | 0.000 | 1 |"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "candidate passes: no comparison is a regression, and every arm has at least 20 judged runs of every scenario"
            ),
            "{text}"
        );
        let text = judged(19, 19, 19);
        assert!(
            text.contains(
                "candidate is inconclusive: no comparison is a regression, but the fewest judged runs on a scenario are 19"
            ),
            "{text}"
        );
        let text = judged(20, 8, 20);
        assert!(
            text.contains("candidate fails: case/reports (must_show) falls from 1.000 to 0.400"),
            "{text}"
        );
        let text = judged(10, 4, 10);
        assert!(
            text.contains("candidate fails: case/reports (must_show) falls from 1.000 to 0.400"),
            "a regression fails below the floor: {text}"
        );
    }

    /// Twenty runs an arm whose every judge call failed are not twenty judged
    /// runs: the probe rows have nothing to compare, so the verdict is
    /// inconclusive and never `pass` (model-prompts §8.1).
    #[test]
    fn runs_whose_judge_calls_all_failed_are_inconclusive() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let mut records = Vec::new();
        for arm in ["current", "candidate"] {
            for _ in 0..20 {
                records.push(record(
                    arm,
                    &scenario,
                    &[("reports", ProbeVerdict::Unparsed)],
                    JudgeOutcome::CallFailed,
                ));
            }
        }
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            &records,
        );
        assert!(
            text.contains("| failed runs | 20/20 (1.000) | 20/20 (1.000) |"),
            "the runs are compared, once, as failed runs: {text}"
        );
        assert!(
            text.contains("| case/reports (must_show) | – | – |"),
            "the probe row has no counted run: {text}"
        );
        assert!(
            text.contains("candidate is inconclusive: no comparison is a regression"),
            "{text}"
        );
        assert!(
            text.contains("case/reports (must_show) has no counted run in an arm"),
            "{text}"
        );
        assert!(
            text.contains("the fewest judged runs on a scenario are 0, under the floor of 20"),
            "{text}"
        );
    }

    /// The same holds when the judge answered unreadably: twenty unjudged
    /// runs compare zero probes with zero probes, and the verdict is
    /// inconclusive (model-prompts §8.1).
    #[test]
    fn runs_the_judge_could_not_read_are_inconclusive() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let mut records = Vec::new();
        for arm in ["current", "candidate"] {
            for _ in 0..20 {
                records.push(record(
                    arm,
                    &scenario,
                    &[("reports", ProbeVerdict::Unparsed)],
                    JudgeOutcome::Unreadable,
                ));
            }
        }
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            &records,
        );
        assert!(
            text.contains("| failed runs | 0/20 (0.000) | 0/20 (0.000) |"),
            "an unjudged run is not a failed run: {text}"
        );
        assert!(
            text.contains("| unjudged runs | 20/20 (1.000) | 20/20 (1.000) |"),
            "{text}"
        );
        assert!(
            text.contains("candidate is inconclusive: no comparison is a regression"),
            "{text}"
        );
        assert!(
            text.contains("case/reports (must_show) has no counted run in an arm"),
            "{text}"
        );
        assert!(
            text.contains("the fewest judged runs on a scenario are 0, under the floor of 20"),
            "{text}"
        );
    }

    /// The floor counts judged runs: nineteen judged runs and one failed run
    /// per arm are nineteen, not twenty, and the verdict is inconclusive
    /// (model-prompts §8.1).
    #[test]
    fn a_failed_run_does_not_count_toward_the_floor() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let mut records = Vec::new();
        for name in ["current", "candidate"] {
            records.extend(arm(&scenario, name, 19, 19));
            records.push(record(
                name,
                &scenario,
                &[("reports", ProbeVerdict::Unparsed)],
                JudgeOutcome::CallFailed,
            ));
        }
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            &records,
        );
        assert!(
            text.contains("| case/reports (must_show) | 19/19 (1.000) | 19/19 (1.000) |"),
            "the failed run leaves the probe rates: {text}"
        );
        assert!(
            text.contains(
                "candidate is inconclusive: no comparison is a regression, but the fewest judged runs on a scenario are 19, under the floor of 20."
            ),
            "{text}"
        );
    }

    /// A probe kind no case carries stays out of the family; only a kind the
    /// case set has, with no counted run in an arm, makes the verdict
    /// inconclusive (model-prompts §8.1).
    #[test]
    fn a_kind_absent_from_every_case_stays_out_of_the_family() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let mut records = arm(&scenario, "current", 20, 20);
        records.extend(arm(&scenario, "candidate", 20, 20));
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            &records,
        );
        assert!(
            !text.contains("pooled must_not_show"),
            "no case carries a must_not_show probe: {text}"
        );
        assert!(text.contains("candidate passes:"), "{text}");
    }

    /// A failed run changes no probe's rate: it leaves the counted runs and is
    /// compared once, in the failed-run rate (model-prompts §8.1).
    #[test]
    fn a_failed_run_changes_no_probe_rate_only_the_failed_run_rate() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let current = metrics(&scenario, &arm(&scenario, "current", 20, 20));
        let mut records = arm(&scenario, "candidate", 20, 20);
        records.push(record(
            "candidate",
            &scenario,
            &[("reports", ProbeVerdict::Unparsed)],
            JudgeOutcome::CallFailed,
        ));
        let candidate = metrics(&scenario, &records);
        let key = ("case".to_owned(), "reports".to_owned());
        assert_eq!(
            candidate.counts(&key),
            ProbeCounts {
                passed: 20,
                runs: 20
            }
        );
        assert_eq!((candidate.failed, candidate.runs), (1, 21));
        let family = comparisons(&current, &candidate, std::slice::from_ref(&scenario));
        let probe_row = family
            .iter()
            .find(|comparison| comparison.name.starts_with("case/reports"))
            .unwrap();
        assert_eq!(probe_row.current, Counts::new(20, 20));
        assert_eq!(probe_row.candidate, Counts::new(20, 20));
        let failed = family
            .iter()
            .find(|comparison| comparison.name == "failed runs")
            .unwrap();
        assert_eq!(failed.current, Counts::new(0, 20));
        assert_eq!(failed.candidate, Counts::new(1, 21));
    }

    /// A candidate that violates a `must_not_show` probe more often fails
    /// through the probe's row and the pooled row beside it, and the report
    /// prints both arms' counts (model-prompts §8.1).
    #[test]
    fn a_violation_rise_fails_through_the_pooled_row() {
        let scenario = scenario(vec![probe("floods", ProbeKind::MustNotShow)]);
        let current: Vec<RunRecord> = (0..20)
            .map(|_| {
                record(
                    "current",
                    &scenario,
                    &[("floods", ProbeVerdict::Absent)],
                    JudgeOutcome::Judged,
                )
            })
            .collect();
        let candidate: Vec<RunRecord> = (0..20)
            .map(|index| {
                record(
                    "candidate",
                    &scenario,
                    &[(
                        "floods",
                        if index < 8 {
                            ProbeVerdict::Present
                        } else {
                            ProbeVerdict::Absent
                        },
                    )],
                    JudgeOutcome::Judged,
                )
            })
            .collect();
        let mut records = current;
        records.extend(candidate);
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            &records,
        );
        assert!(
            text.contains("| case/floods (must_not_show) | 0/20 (0.000) | 8/20 (0.400) |"),
            "{text}"
        );
        assert!(
            text.contains("| pooled must_not_show (violations) | 0/20 (0.000) | 8/20 (0.400) |"),
            "{text}"
        );
        assert!(
            text.contains("candidate fails: "),
            "a violation rise is a regression: {text}"
        );
        assert!(text.contains("floods (must_not_show) rises from"), "{text}");
    }

    /// A candidate whose runs the judge answered less often fails through the
    /// unjudged-run comparison, and those runs stay out of the probe rates
    /// (model-prompts §8.1).
    #[test]
    fn an_unjudged_rise_fails_through_the_unjudged_run_row() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let mut records = arm(&scenario, "current", 20, 20);
        records.extend(arm(&scenario, "candidate", 12, 12));
        for _ in 0..8 {
            let mut run = record(
                "candidate",
                &scenario,
                &[("reports", ProbeVerdict::Unparsed)],
                JudgeOutcome::Unreadable,
            );
            run.judge_error = Some("the judge answered with no usable verdicts".into());
            records.push(run);
        }
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            &records,
        );
        assert!(
            text.contains("| case/reports (must_show) | 20/20 (1.000) | 12/12 (1.000) |"),
            "an unjudged run is out of the probe rates: {text}"
        );
        assert!(
            text.contains("| unjudged runs | 0/20 (0.000) | 8/20 (0.400) |"),
            "{text}"
        );
        assert!(text.contains("unjudged runs rises from"), "{text}");
    }

    /// The turn account is the export's, not the notice stream's: a turn that
    /// ended while the idle probe waited is counted, and its failure still
    /// fails the run.
    #[test]
    fn turn_accounting_follows_the_export() {
        let export = [
            r#"{"kind":"header","export":1}"#,
            r#"{"kind":"branch","info":{"id":"main"},"head":4}"#,
            r#"{"kind":"event","branch_id":"main","event":{"sequence":1,"type":"turn","name":"start","data":{}}}"#,
            r#"{"kind":"event","branch_id":"main","event":{"sequence":2,"type":"turn","name":"end","data":{"status":"completed"}}}"#,
            r#"{"kind":"event","branch_id":"main","event":{"sequence":3,"type":"turn","name":"start","data":{}}}"#,
            r#"{"kind":"event","branch_id":"main","event":{"sequence":4,"type":"turn","name":"end","data":{"status":"failed","error":"provider error"}}}"#,
            r#"{"kind":"end"}"#,
        ]
        .join("\n");
        assert_eq!(
            turns_in_export(&export),
            (2, Some("main: the turn failed: provider error".to_owned()))
        );
        assert_eq!(turns_in_export(""), (0, None));
    }

    /// A declared tool without a source cannot come from the product: the
    /// request holds `ToolDefinition` values, whose `source` is a required
    /// field. The probe fails naming the tool instead of dropping it from
    /// the ceiling, where it would leave the `tools-product` comparison
    /// silently uncompared.
    #[test]
    fn requested_tools_names_a_declared_tool_without_a_source() {
        let line = |tools: &str| {
            format!(
                r#"{{"kind":"event","branch_id":"main","event":{{"type":"model","name":"call","data":{{"request":{{"tools":{tools}}}}}}}}}"#
            )
        };
        let export = [
            line(r#"[{"name":"read","source":"builtin"},{"name":"write","source":"builtin"}]"#),
            // The last request is the one the probe's ceiling comes from.
            line(r#"[{"name":"read","source":"builtin"},{"name":"write"}]"#),
        ]
        .join("\n");
        let error = requested_tools(&export).expect_err("the missing source fails the probe");
        assert!(error.contains("write"), "the error names the tool: {error}");
    }

    /// Every request the judge reads can be rebuilt from its rendered form,
    /// whatever fields the request type carries: a request with a capability
    /// snapshot, request metadata and a field added after the renderer was
    /// written comes back whole, because the renderer starts from the request
    /// itself and replaces only what it renders. The view also says each
    /// request's message count, and a branch's first request is given in full
    /// against its own line's previous request, never another line's (issue
    /// #635).
    ///
    /// A dedicated test, because no flow produces these fields: both of the
    /// runner's flows serve a development model configuration, whose
    /// selections carry no capability snapshot and no metadata. The flows
    /// assert the same rule on the requests they do produce.
    #[test]
    fn the_judge_view_keeps_every_request_field() {
        fn request(messages: &[&str]) -> Value {
            json!({
                "providerId": "p",
                "modelId": "m",
                "capabilitySnapshot": {
                    "target": {"providerId": "p", "modelId": "m"},
                    "capabilities": {"contextWindow": 128_000, "maxOutputTokens": 16_384},
                    "reasoning": null,
                    "serviceTier": null,
                },
                "messages": messages
                    .iter()
                    .map(|text| json!({"role": "user", "content": text}))
                    .collect::<Vec<_>>(),
                "system": "S",
                "tools": [{"name": "read"}],
                "config": {"maxTokens": 8},
                "metadata": {"sessionId": "s", "runId": "r", "providerProtocol": "openai-chat"},
                "aFieldAddedLater": {"kept": true},
            })
        }
        let line = |branch: &str, request: Value| {
            json!({
                "kind": "event",
                "branch_id": branch,
                "event": {"type": "model", "name": "call", "data": {"request": request}},
            })
            .to_string()
        };
        let export = [
            r#"{"kind":"header","export":1}"#.to_owned(),
            line("main", request(&["one"])),
            line("main", request(&["one", "two"])),
            // The other line's first request is the one `main` made last, so
            // only the line it belongs to says it was given in full.
            line("other", request(&["one", "two"])),
            r#"{"kind":"end","branches":2,"events":3}"#.to_owned(),
        ]
        .join("\n");
        let view = judge_view(&export);
        let rebuilt = rebuilt_requests(&view);
        assert_eq!(rebuilt.len(), 3, "every request is in the view");
        for (index, request) in rebuilt {
            let original: Value = serde_json::from_str(export.lines().nth(index).unwrap()).unwrap();
            assert_eq!(
                request, original["event"]["data"]["request"],
                "the request at line {index} is rebuilt from the view"
            );
        }
        let other: Value = view
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|line| line["branch_id"] == "other")
            .unwrap();
        assert!(
            other["event"]["data"]["request"]["messages"].is_array(),
            "the other line's first request is given in full: {other}"
        );
    }

    /// The requests [`judge_view`] rendered, rebuilt from the view alone: the
    /// line each came from, and the request its export carried. A field
    /// rendered as `{"unchanged": true}` takes the previous request's value on
    /// its line, `addedMessages` continues the previous request's list, and
    /// the view's message count must be the rebuilt list's.
    fn rebuilt_requests(view: &str) -> Vec<(usize, Value)> {
        let mut previous: BTreeMap<String, Value> = BTreeMap::new();
        let mut rebuilt = Vec::new();
        for (index, raw) in view.lines().enumerate() {
            let Ok(line) = serde_json::from_str::<Value>(raw) else {
                continue;
            };
            let Some(rendered) = line["event"]["data"]["request"].as_object() else {
                continue;
            };
            let branch = line["branch_id"].as_str().unwrap_or_default().to_owned();
            let earlier = previous.get(&branch).cloned();
            let mut request = serde_json::Map::new();
            let mut messages = None;
            let mut counted = None;
            for (field, value) in rendered {
                let unchanged = value.as_object().is_some_and(|marker| {
                    marker.len() == 1 && marker.get("unchanged") == Some(&json!(true))
                });
                match field.as_str() {
                    "messageCount" => counted = value.as_u64(),
                    "addedMessages" => {
                        let mut all = earlier
                            .as_ref()
                            .and_then(|earlier| earlier["messages"].as_array().cloned())
                            .unwrap_or_default();
                        all.extend(value.as_array().cloned().unwrap_or_default());
                        messages = Some(Value::Array(all));
                    }
                    "messages" => messages = Some(value.clone()),
                    _ if unchanged => {
                        request.insert(
                            field.clone(),
                            earlier
                                .as_ref()
                                .and_then(|earlier| earlier.get(field))
                                .cloned()
                                .unwrap_or(Value::Null),
                        );
                    }
                    _ => {
                        request.insert(field.clone(), value.clone());
                    }
                }
            }
            if let Some(messages) = messages {
                request.insert("messages".to_owned(), messages);
            }
            assert_eq!(
                request
                    .get("messages")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len) as u64,
                counted.expect("the view says each request's message count"),
                "the message count is the rebuilt request's"
            );
            let request = Value::Object(request);
            previous.insert(branch, request.clone());
            rebuilt.push((index, request));
        }
        rebuilt
    }

    /// A run that stopped on its wall-clock budget is a failed run, scored as
    /// before, even when its judge gave no answer: only the run's own failure
    /// would leave it out (issue #642).
    #[test]
    fn a_run_that_stopped_on_its_wall_clock_budget_stays_a_failed_run() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let mut record = record(
            "candidate",
            &scenario,
            &[("reports", ProbeVerdict::Unparsed)],
            JudgeOutcome::Unreadable,
        );
        // The record a run that stopped on its budget is built as.
        record.stop = "seconds";
        record.failed = failed("seconds", None, record.judge_outcome);
        record.judge_error = Some("the judge answered with no usable verdicts: no answer".into());
        assert!(!record.unjudged(), "the run failed on its own");
        let metrics = metrics(&scenario, std::slice::from_ref(&record));
        assert_eq!(metrics.failed, 1);
        assert_eq!(metrics.unjudged, 0);
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            std::slice::from_ref(&record),
        );
        assert!(
            text.contains("| failed runs | – | 1/1 (1.000) |"),
            "the run is compared as a failed run: {text}"
        );
        assert!(
            !text.contains("| arm | scenario | run | judge error |"),
            "{text}"
        );
    }

    /// The report counts each arm's unjudged runs, lists them with the first
    /// line of the judge error, and adds no such row when there are none
    /// (issue #642).
    #[test]
    fn the_report_lists_unjudged_runs_with_the_first_line_of_the_judge_error() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let mut unjudged = record(
            "candidate",
            &scenario,
            &[("reports", ProbeVerdict::Unparsed)],
            JudgeOutcome::Unreadable,
        );
        unjudged.judge_error =
            Some("the judge answered with no usable verdicts: no answer\nand the retry too".into());
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            std::slice::from_ref(&unjudged),
        );
        assert!(
            text.contains("| unjudged runs | – | 1/1 (1.000) |"),
            "the unjudged-run rate counts it: {text}"
        );
        assert!(
            text.contains(
                "| candidate | case | 0 | the judge answered with no usable verdicts: no answer |"
            ),
            "{text}"
        );
        assert!(!text.contains("and the retry too"), "{text}");
        let judged = record(
            "candidate",
            &scenario,
            &[("reports", ProbeVerdict::Present)],
            JudgeOutcome::Judged,
        );
        let text = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            std::slice::from_ref(&judged),
        );
        assert!(
            !text.contains("| arm | scenario | run | judge error |"),
            "{text}"
        );
    }

    /// The report names the runs its judge counts are over, and shows the
    /// re-asks over every run beside the dropped-temperature count
    /// (model-prompts §8.1).
    #[test]
    fn the_report_counts_asked_runs_and_re_asks() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let run = record(
            "current",
            &scenario,
            &[("reports", ProbeVerdict::Present)],
            JudgeOutcome::Judged,
        );
        let quiet = report(
            &instrument(model()),
            std::slice::from_ref(&scenario),
            std::slice::from_ref(&run),
        );
        assert!(!quiet.contains("| judge temperature |"), "{quiet}");
        assert!(!quiet.contains("| judge re-asks |"), "{quiet}");
        let mut instrument = instrument(model());
        instrument.judge_asked_runs = 3;
        instrument.judge_temperature_dropped = 2;
        instrument.judge_reasks = 5;
        let text = report(
            &instrument,
            std::slice::from_ref(&scenario),
            std::slice::from_ref(&run),
        );
        assert!(
            text.contains("| judge temperature | dropped in 2 of 3 runs the judge was asked |"),
            "{text}"
        );
        assert!(
            text.contains("| judge re-asks | 5 across all runs |"),
            "{text}"
        );
    }

    /// The report shows the resends the runs made, and no row when no run
    /// made one.
    #[test]
    fn the_report_shows_decline_retries_only_when_a_run_sent_one() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let quiet = record(
            "current",
            &scenario,
            &[("reports", ProbeVerdict::Present)],
            JudgeOutcome::Judged,
        );
        let mut measured = instrument(model());
        measured.merge(&quiet, instrument(model()));
        let text = report(
            &measured,
            std::slice::from_ref(&scenario),
            std::slice::from_ref(&quiet),
        );
        assert!(!text.contains("| decline retries |"), "{text}");
        let mut retried = record(
            "candidate",
            &scenario,
            &[("reports", ProbeVerdict::Present)],
            JudgeOutcome::Judged,
        );
        retried.decline_retries = Some(2);
        let mut measured = instrument(model());
        measured.merge(&retried, instrument(model()));
        let text = report(
            &measured,
            std::slice::from_ref(&scenario),
            std::slice::from_ref(&retried),
        );
        assert!(
            text.contains("| decline retries | 2 across all runs |"),
            "{text}"
        );
    }

    struct Local;
    impl ResolveModelConnection for Local {
        fn resolve(
            &self,
            _: Arc<ModelRequest>,
            _: CancellationToken,
        ) -> HostFuture<'_, ResolvedModelConnection> {
            Box::pin(async {
                Ok(ResolvedModelConnection {
                    connection: ModelConnection::Local,
                    failure_policy: None,
                    recovery: None,
                })
            })
        }
    }
    /// Answers every judge call with prose that holds no verdict list, and
    /// counts the calls it was asked.
    #[derive(Default)]
    struct Garbage(AtomicUsize);
    impl ModelDriver for Garbage {
        fn invoke(
            &self,
            _: Arc<ModelRequest>,
            _: ModelConnection,
            _: CancellationToken,
        ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(dim_agent_capabilities::host::ModelChunk::MessageStart {
                        message_id: "reply".into(),
                    }),
                    Ok(dim_agent_capabilities::host::ModelChunk::Text(
                        "the run looks fine to me".into(),
                    )),
                    Ok(dim_agent_capabilities::host::ModelChunk::MessageStop {
                        message_id: "reply".into(),
                        reason: ModelStopReason::EndTurn,
                    }),
                ]))
                    as dim_agent_capabilities::host::ModelStream)
            })
        }
    }

    /// A judge that never answers in a readable form is asked three times, and
    /// the run is unjudged: not a failed run, out of its arm's metrics, and
    /// listed in the report (issue #642, acceptance 3).
    #[tokio::test]
    async fn a_judge_that_never_answers_readably_leaves_the_run_unjudged() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let judge = ModelSelection {
            provider_id: "fake".into(),
            model_id: "fake".into(),
            capability_snapshot: None,
            config: None,
            metadata: None,
        };
        let driver = Arc::new(Garbage::default());
        let judging = judge_export(
            &Arc::new(ModelHost(driver.clone())),
            &Arc::new(ProviderConnectionResolver(Arc::new(Local))),
            &judge,
            &scenario,
            "{\"kind\":\"header\"}",
            JudgeWindow {
                window: 8_000,
                max_output: 1_024,
            },
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            driver.0.load(Ordering::Relaxed),
            3,
            "the call and its two re-asks"
        );
        assert_eq!(judging.outcome, JudgeOutcome::Unreadable);
        assert_eq!(judging.reasks, 2);
        assert!(
            judging
                .probes
                .iter()
                .all(|probe| probe.verdict == ProbeVerdict::Unparsed)
        );
        let error = judging
            .error
            .expect("an unusable answer is the judge's error");
        assert!(
            error.starts_with(
                "the judge answered with no usable verdicts: it is not a JSON array of verdict objects ("
            ),
            "{error}"
        );
        let record = RunRecord {
            judge_outcome: judging.outcome,
            judge_error: Some(error),
            probes: judging.probes,
            ..record("current", &scenario, &[], JudgeOutcome::Judged)
        };
        assert!(!record.failed, "an unjudged run is not a failed run");
        let serialized = serde_json::to_value(&record).unwrap();
        assert_eq!(serialized["failed"], false, "{serialized}");
        assert_eq!(serialized["judgeOutcome"], "unreadable", "{serialized}");
        let metrics = metrics(&scenario, std::slice::from_ref(&record));
        assert_eq!(metrics.unjudged, 1);
        assert_eq!(metrics.failed, 0);
        assert_eq!(
            metrics.counts(&("case".to_owned(), "reports".to_owned())),
            ProbeCounts::default(),
            "an unjudged run answers no probe"
        );
        let text = report(
            &instrument(ModelId {
                provider_id: "fake".into(),
                model_id: "fake".into(),
            }),
            std::slice::from_ref(&scenario),
            std::slice::from_ref(&record),
        );
        assert!(
            text.contains("| current | case | 0 | the judge answered with no usable verdicts: it is not a JSON array of verdict objects ("),
            "{text}"
        );
    }

    /// Answers with a thinking block, no text, and a `max_tokens` stop at a
    /// 384,000-token output cap.
    #[derive(Default)]
    struct Capped(AtomicUsize);
    impl ModelDriver for Capped {
        fn invoke(
            &self,
            _: Arc<ModelRequest>,
            _: ModelConnection,
            _: CancellationToken,
        ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(dim_agent_capabilities::host::ModelChunk::MessageStart {
                        message_id: "reply".into(),
                    }),
                    Ok(dim_agent_capabilities::host::ModelChunk::Thinking(
                        "weighing the timeline".into(),
                    )),
                    Ok(dim_agent_capabilities::host::ModelChunk::Usage(
                        dim_agent_types::Usage {
                            prompt_tokens: 1_000.,
                            completion_tokens: 384_000.,
                            total_tokens: 385_000.,
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                            reasoning_tokens: None,
                            cost: None,
                            currency: None,
                        },
                    )),
                    Ok(dim_agent_capabilities::host::ModelChunk::MessageStop {
                        message_id: "reply".into(),
                        reason: ModelStopReason::MaxTokens,
                    }),
                ]))
                    as dim_agent_capabilities::host::ModelStream)
            })
        }
    }

    /// A judge reply cut off at its output cap is a failed call, not an
    /// unreadable answer: the run is a failed run, the error names the cap,
    /// and there is no re-ask.
    #[tokio::test]
    async fn a_judge_cut_off_at_its_output_cap_is_a_failed_call() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let judge = ModelSelection {
            provider_id: "fake".into(),
            model_id: "fake".into(),
            capability_snapshot: None,
            config: None,
            metadata: None,
        };
        let driver = Arc::new(Capped::default());
        let judging = judge_export(
            &Arc::new(ModelHost(driver.clone())),
            &Arc::new(ProviderConnectionResolver(Arc::new(Local))),
            &judge,
            &scenario,
            "{\"kind\":\"header\"}",
            JudgeWindow {
                window: 1_048_576,
                max_output: 384_000,
            },
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            driver.0.load(Ordering::Relaxed),
            1,
            "a cap stop is not asked again"
        );
        assert_eq!(judging.outcome, JudgeOutcome::CallFailed);
        assert_eq!(judging.reasks, 0);
        let error = judging.error.expect("a cap stop is the judge's error");
        assert!(
            error.starts_with("the judge stopped at its output cap"),
            "{error}"
        );
        assert!(error.contains("384000"), "{error}");
        assert!(
            judging
                .probes
                .iter()
                .all(|probe| probe.verdict == ProbeVerdict::Unparsed)
        );
        assert_eq!(judging.calls.len(), 1, "one call is recorded");
        let call = &judging.calls[0];
        assert_eq!(call.stop, "max_tokens");
        assert_eq!(call.text_bytes, 0);
        assert!(call.thinking_bytes > 0, "the thinking deltas are counted");
        assert_eq!(call.completion_tokens, Some(384_000.));
        let record = RunRecord {
            failed: failed("idle", None, judging.outcome),
            judge_outcome: judging.outcome,
            judge_error: Some(error),
            judge_reasks: judging.reasks,
            judge_calls: judging.calls,
            probes: judging.probes,
            ..record("current", &scenario, &[], JudgeOutcome::Judged)
        };
        assert!(record.failed, "a failed judge call is a failed run");
        let serialized = serde_json::to_value(&record).unwrap();
        assert_eq!(serialized["judgeOutcome"], "call_failed", "{serialized}");
        assert_eq!(
            serialized["judgeCalls"][0]["stop"], "max_tokens",
            "{serialized}"
        );
    }

    /// The judge's window is its selection's capability snapshot, or else the
    /// model configuration's own declaration; with neither, the evaluation
    /// fails before the first call and names the judge (issue #635).
    ///
    /// A dedicated test, because no flow can reach the failure: a model
    /// configuration requires a `contextWindow` for every model, so a judge
    /// that resolves from one always declares a window, and the product
    /// profile's selections all carry a snapshot (the catalog's assumed pair
    /// where nothing is declared, #522).
    #[tokio::test]
    async fn a_judge_without_a_declared_window_fails_the_evaluation() {
        let judge = ModelSelection {
            provider_id: "gateway".into(),
            model_id: "unknown".into(),
            capability_snapshot: None,
            config: None,
            metadata: None,
        };
        let error = judge_window(&judge, None, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("gateway/unknown"), "{error}");
        assert!(error.contains("declares no context window"), "{error}");
        let target = ModelId {
            provider_id: "gateway".into(),
            model_id: "unknown".into(),
        };
        let declared = ModelSelection {
            capability_snapshot: Some(dim_agent_types::ModelCapabilitySnapshot {
                target,
                capabilities: dim_agent_types::ModelCapabilities {
                    context_window: 64_000,
                    max_output_tokens: 200,
                    ..dim_agent_types::ModelCapabilities::with_limits(64_000, 200)
                },
                reasoning: None,
                service_tier: None,
            }),
            ..judge
        };
        let limits = judge_window(&declared, None, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!((limits.window, limits.max_output), (64_000, 200));
    }

    /// The judge answers one verdict object per line: one request per run
    /// reads both probes' verdicts (issue #642, acceptance 1).
    fn one_object_per_line(text: &str) -> Option<Value> {
        text.contains("You check statements").then(|| {
            content(
                "{\"id\": \"ran\", \"verdict\": \"present\", \"quote\": \"turn/end\"}\n\
                 {\"id\": \"floods\", \"verdict\": \"absent\", \"quote\": \"nothing\"}",
            )
        })
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_judge_answering_one_object_per_line_is_read_in_one_request() {
        let fixture = Fixture::start(
            "per-line",
            one_object_per_line,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [
                    {"id": "ran", "kind": "must_show", "text": "the run has a transcript"},
                    {"id": "floods", "kind": "must_not_show", "text": "the run floods the Timeline"}
                ]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
        assert!(
            report.contains("| text | `context-compaction/summary` |"),
            "a run's report names the texts under test: {report}"
        );
        assert_eq!(fixture.judge().len(), 2, "one judge request per arm");
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["judgeReasks"], 0, "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
            assert_eq!(record["probes"][0]["verdict"], "present", "{record}");
            assert_eq!(record["probes"][1]["verdict"], "absent", "{record}");
            let calls = record["judgeCalls"]
                .as_array()
                .expect("every record lists its judge calls");
            assert_eq!(calls.len(), 1, "one answered call: {record}");
            assert_eq!(calls[0]["stop"], "end_turn", "{record}");
            assert!(
                calls[0]["textBytes"].as_u64().unwrap() > 0,
                "the reply's text is measured: {record}"
            );
            assert!(
                calls[0]["thinkingBytes"].as_u64().is_some(),
                "the reasoning deltas are measured: {record}"
            );
            assert_eq!(
                record["temperatureDropped"],
                json!(false),
                "the record says whether the judge dropped its temperature: {record}"
            );
        }
    }

    /// The judge's verdict for the `must_not_show` probe, explaining why the
    /// run does not show it.
    fn contradicts_the_probe(text: &str) -> Option<Value> {
        text.contains("You check statements")
            .then(|| content(r#"[{"id":"floods","verdict":"contradicted","quote":"turn/end"}]"#))
    }
    /// A `contradicted` verdict is kept as it is, passes a `must_not_show`
    /// probe with the scores unchanged, and is no violation (model-prompts
    /// §8.1). The parser the scenario judge shares with the summary
    /// evaluation accepts it although the scenario judge is asked for
    /// `present` or `absent`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_contradicted_verdict_passes_a_must_not_show_probe() {
        let fixture = Fixture::start(
            "contradicted",
            contradicts_the_probe,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "floods", "kind": "must_not_show", "text": "the run floods the Timeline"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
            assert_eq!(record["probes"][0]["verdict"], "contradicted", "{record}");
            assert_eq!(record["failed"], false, "{record}");
        }
        assert!(
            report.contains("| case/floods (must_not_show) | 0/1 (0.000) | 0/1 (0.000) |"),
            "a contradicted verdict is no violation: {report}"
        );
        assert!(
            report.contains("| pooled must_not_show (violations) | 0/1 (0.000) | 0/1 (0.000) |"),
            "{report}"
        );
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
    }

    /// The judge's first answer loses a level of escaping in a quote, as one
    /// answer in the evaluation for #553 did, so it is not JSON.
    const UNESCAPED: &str =
        r#"[{"id": "ran", "verdict": "present", "quote": "turn/end: the run said "hello""}]"#;
    /// The judge's first answer is [`UNESCAPED`], and every answer after a
    /// re-ask is readable.
    fn unescaped_then_readable(text: &str) -> Option<Value> {
        text.contains("You check statements").then(|| {
            content(if text.contains("Your answer could not be used") {
                "[{\"id\": \"ran\", \"verdict\": \"present\", \"quote\": \"turn/end\"}]"
            } else {
                UNESCAPED
            })
        })
    }
    /// A judge answer that does not read is asked again, and the second answer
    /// is the one used (issue #642, acceptance 2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_reply_that_does_not_read_is_asked_again() {
        let fixture = Fixture::start(
            "re-ask",
            unescaped_then_readable,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["judgeReasks"], 1, "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
            assert_eq!(record["probes"][0]["verdict"], "present", "{record}");
            let calls = record["judgeCalls"]
                .as_array()
                .expect("every record lists its judge calls");
            assert_eq!(calls.len(), 2, "the call and its re-ask: {record}");
            for call in calls {
                assert_eq!(call["stop"], "end_turn", "{record}");
            }
        }
        let judge = fixture.judge();
        assert_eq!(judge.len(), 4, "one call and one re-ask per arm");
        for pair in judge.chunks(2) {
            assert!(
                !pair[0].contains("Your answer could not be used"),
                "the first call carries no re-ask"
            );
            assert!(
                pair[1].contains("the run said"),
                "the re-ask carries the first answer: {}",
                pair[1]
            );
            assert!(
                pair[1].contains(
                    "Your answer could not be used: it is not a JSON array of verdict objects ("
                ),
                "the re-ask says what was wrong: {}",
                pair[1]
            );
            assert!(
                pair[1].contains("\"role\":\"assistant\""),
                "the first answer is carried as an assistant message: {}",
                pair[1]
            );
        }
    }

    /// A re-judge reads each saved run's `judge-view.jsonl` as its view,
    /// keeps the run's own outcome, and leaves a run that failed before
    /// judging alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_rejudge_judges_the_selected_saved_runs_from_their_views() {
        let fixture = Fixture::start(
            "rejudge",
            judged,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let source = fixture.directory.join("source");
        for (arm, run, view) in [
            ("current", 0, "SAVED VIEW unjudged"),
            ("current", 1, "SAVED VIEW judged"),
            ("candidate", 1, "SAVED VIEW unselected"),
        ] {
            let directory = source.join("outputs").join(arm).join("case");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(format!("run-{run}.judge-view.jsonl")), view).unwrap();
        }
        let saved = [
            json!({"arm": "current", "scenario": "case", "run": 0, "session": null,
                   "stop": "idle", "turns": 2, "window": 24000.0, "durationMs": 1.0,
                   "failed": false, "judgeOutcome": "unreadable", "error": null,
                   "judgeError": "the judge answered with no usable verdicts", "judgeReasks": 2}),
            json!({"arm": "current", "scenario": "case", "run": 1, "session": null,
                   "stop": "idle", "turns": 3, "window": 24000.0, "durationMs": 1.0,
                   "failed": false, "judgeOutcome": "judged", "error": null,
                   "judgeError": null, "judgeReasks": 0, "declineRetries": 3}),
            json!({"arm": "candidate", "scenario": "case", "run": 0, "session": null,
                   "stop": "error", "turns": 0, "window": null, "durationMs": 1.0,
                   "failed": true, "judgeOutcome": "not_asked",
                   "error": "the run failed before judging", "judgeError": null,
                   "judgeReasks": 0}),
            json!({"arm": "candidate", "scenario": "case", "run": 1, "session": null,
                   "stop": "idle", "turns": 2, "window": 24000.0, "durationMs": 1.0,
                   "failed": false, "judgeOutcome": "judged", "error": null,
                   "judgeError": null, "judgeReasks": 0}),
        ];
        let jsonl: String = saved.iter().map(|record| format!("{record}\n")).collect();
        std::fs::write(source.join("runs.jsonl"), &jsonl).unwrap();
        let out = fixture.directory.join("rejudged");
        let mut options = fixture.options(out.clone());
        options.rejudge = Some(source.clone());
        options.select = vec![
            "current/case/run-0".into(),
            "current/case/run-1".into(),
            "candidate/case/run-0".into(),
        ];
        let report = fixture.rejudge(&options).await.unwrap();
        assert!(report.contains("| judge |"), "{report}");
        assert!(
            report.contains(&format!(
                "| sources | `{}` (3 runs) |",
                source.canonicalize().unwrap().display()
            )),
            "the report names the saved runs' source: {report}"
        );
        assert!(
            !report.contains("| text |"),
            "no saved run records the texts under test: {report}"
        );
        assert!(
            report.contains("| decline retries | 3 across all runs; not recorded for 2 runs |"),
            "a saved run written before the runner counted declines is not read as none: {report}"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("runs.jsonl")).unwrap(),
            jsonl,
            "the source is never written to"
        );
        assert!(out.join("scenario.md").exists());
        let records = fixture.records(&out);
        assert_eq!(records.len(), 3, "one record per selected run");
        let previously_unjudged = &records[0];
        assert_eq!(
            previously_unjudged["declineRetries"],
            Value::Null,
            "a re-judged run keeps an unrecorded count unrecorded: {previously_unjudged}"
        );
        assert_eq!(
            records[1]["declineRetries"], 3,
            "a re-judged run keeps the count its source recorded: {}",
            records[1]
        );
        assert_eq!(
            previously_unjudged["arm"], "current",
            "{previously_unjudged}"
        );
        assert_eq!(
            previously_unjudged["judgeOutcome"], "judged",
            "{previously_unjudged}"
        );
        assert_eq!(
            previously_unjudged["probes"][0]["verdict"], "present",
            "{previously_unjudged}"
        );
        assert_eq!(
            previously_unjudged["stop"], "idle",
            "the run's own outcome is kept"
        );
        assert_eq!(previously_unjudged["turns"], 2, "{previously_unjudged}");
        assert_eq!(
            previously_unjudged["judgeCalls"].as_array().unwrap().len(),
            1,
            "{previously_unjudged}"
        );
        assert_eq!(
            records[1]["judgeOutcome"], "judged",
            "a judged run is judged again: {}",
            records[1]
        );
        assert_eq!(records[1]["turns"], 3, "the run's turns are kept");
        let skipped = &records[2];
        assert_eq!(skipped["judgeOutcome"], "not_asked", "{skipped}");
        assert_eq!(skipped["failed"], true, "{skipped}");
        assert_eq!(
            skipped["error"], "the run failed before judging",
            "{skipped}"
        );
        assert_eq!(
            skipped["judgeCalls"].as_array().unwrap().len(),
            0,
            "a run that failed before judging is not judged: {skipped}"
        );
        let asked = fixture.judge();
        assert_eq!(asked.len(), 2, "one call per selected run with a view");
        assert!(asked[0].contains("SAVED VIEW unjudged"), "{}", asked[0]);
        assert!(asked[1].contains("SAVED VIEW judged"), "{}", asked[1]);
        assert!(
            asked
                .iter()
                .all(|request| !request.contains("SAVED VIEW unselected")),
            "a run --run did not select is not judged: {asked:?}"
        );
    }

    /// A pool reports several finished evaluations under one rule: both
    /// sources' runs are added, their probes counted together, and no source
    /// is written to.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_pool_reports_several_finished_evaluations_under_one_rule() {
        let fixture = Fixture::start(
            "pool",
            judged,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let source = |name: &str, current: Value, candidate: Value, dropped: Option<[bool; 2]>| {
            let directory = fixture.directory.join(name);
            std::fs::create_dir_all(&directory).unwrap();
            let mut runs = [
                json!({"arm": "current", "scenario": "case", "run": 0, "session": null,
                       "stop": "idle", "turns": 2, "window": 24000.0, "durationMs": 1.0,
                       "failed": false, "judgeOutcome": "judged", "error": null,
                       "judgeError": null, "judgeReasks": 0,
                       "probes": [{"id": "ran", "kind": "must_show",
                                   "verdict": current, "quote": null}]}),
                json!({"arm": "candidate", "scenario": "case", "run": 0, "session": null,
                       "stop": "idle", "turns": 2, "window": 24000.0, "durationMs": 1.0,
                       "failed": false, "judgeOutcome": "judged", "error": null,
                       "judgeError": null, "judgeReasks": 0,
                       "probes": [{"id": "ran", "kind": "must_show",
                                   "verdict": candidate, "quote": null}]}),
            ];
            if let Some([current, candidate]) = dropped {
                runs[0]["temperatureDropped"] = json!(current);
                runs[1]["temperatureDropped"] = json!(candidate);
            }
            let jsonl: String = runs.iter().map(|record| format!("{record}\n")).collect();
            std::fs::write(directory.join("runs.jsonl"), &jsonl).unwrap();
            (directory, jsonl)
        };
        let (first, first_jsonl) = source(
            "first",
            json!("present"),
            json!("absent"),
            Some([true, false]),
        );
        let (second, second_jsonl) = source("second", json!("present"), json!("present"), None);
        let out = fixture.directory.join("pooled");
        let mut options = fixture.options(out.clone());
        options.pool = vec![first.clone(), second.clone()];
        let report = fixture.pooled(&options).await.unwrap();
        assert!(
            report.contains("| runs | 2 per arm and scenario |"),
            "{report}"
        );
        assert!(
            report.contains("| case/ran (must_show) | 2/2 (1.000) | 1/2 (0.500) |"),
            "{report}"
        );
        assert!(
            report.contains(
                "| judge temperature | dropped in 1 of 4 runs the judge was asked; not recorded for 2 runs |"
            ),
            "the row counts what is recorded and says what is not: {report}"
        );
        assert!(
            report.contains(&format!(
                "| sources | `{}` (2 runs), `{}` (2 runs); the model and judge rows are those \
                 given to --pool; each source's scenario.md states the instrument its runs were \
                 judged with |",
                first.canonicalize().unwrap().display(),
                second.canonicalize().unwrap().display()
            )),
            "the report names both sources and the limits of its instrument: {report}"
        );
        assert!(
            !report.contains("| text |"),
            "no saved run records the texts under test: {report}"
        );
        assert_eq!(
            std::fs::read_to_string(first.join("runs.jsonl")).unwrap(),
            first_jsonl,
            "the first source is never written to"
        );
        assert_eq!(
            std::fs::read_to_string(second.join("runs.jsonl")).unwrap(),
            second_jsonl,
            "the second source is never written to"
        );
        let records = fixture.records(&out);
        assert_eq!(
            records.len(),
            4,
            "every run of both sources is kept: {records:?}"
        );
        assert!(out.join("scenario.md").exists());
    }

    /// A report over saved runs counts the declines their records carry and
    /// says how many records carry none: a record written before the runner
    /// counted them is not read as no declines.
    #[test]
    fn a_pool_report_counts_runs_that_did_not_record_decline_retries() {
        let directory = std::env::temp_dir().join(format!(
            "dim-scenario-eval-decline-retries-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let runs = [
            json!({"arm": "current", "scenario": "case", "run": 0, "session": null,
                   "stop": "idle", "turns": 2, "window": 8000.0, "durationMs": 1.0,
                   "failed": false, "judgeOutcome": "judged", "error": null,
                   "judgeError": null, "judgeReasks": 0, "declineRetries": 2,
                   "probes": [{"id": "ran", "kind": "must_show",
                               "verdict": "present", "quote": null}]}),
            json!({"arm": "candidate", "scenario": "case", "run": 0, "session": null,
                   "stop": "idle", "turns": 2, "window": 8000.0, "durationMs": 1.0,
                   "failed": false, "judgeOutcome": "judged", "error": null,
                   "judgeError": null, "judgeReasks": 0,
                   "probes": [{"id": "ran", "kind": "must_show",
                               "verdict": "present", "quote": null}]}),
        ];
        let jsonl: String = runs.iter().map(|record| format!("{record}\n")).collect();
        std::fs::write(directory.join("runs.jsonl"), &jsonl).unwrap();
        let scenarios = vec![scenario(vec![probe("ran", ProbeKind::MustShow)])];
        let records = read_pooled_runs(&directory, &scenarios).unwrap();
        let mut instrument = instrument(model());
        let measured = instrument.clone();
        for record in &records {
            instrument.merge(record, measured.clone());
        }
        let text = report(&instrument, &scenarios, &records);
        assert!(
            text.contains("| decline retries | 2 across all runs; not recorded for 1 runs |"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A pooled run's probes keep the kind their source recorded: a probe
    /// that changed kind between the source run and the pool, or a saved
    /// record without one, is refused rather than reinterpreted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_pool_refuses_a_probe_whose_kind_changed() {
        let fixture = Fixture::start(
            "pool-kind",
            judged,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let source = |name: &str, probe: Value| {
            let directory = fixture.directory.join(name);
            std::fs::create_dir_all(&directory).unwrap();
            let run = json!({"arm": "current", "scenario": "case", "run": 0, "session": null,
                             "stop": "idle", "turns": 2, "window": 24000.0, "durationMs": 1.0,
                             "failed": false, "judgeOutcome": "judged", "error": null,
                             "judgeError": null, "judgeReasks": 0, "probes": [probe]});
            std::fs::write(directory.join("runs.jsonl"), format!("{run}\n")).unwrap();
            directory
        };
        let changed = source(
            "changed",
            json!({"id": "ran", "kind": "must_not_show", "verdict": "present", "quote": null}),
        );
        let mut options = fixture.options(fixture.directory.join("pooled"));
        options.pool = vec![changed];
        let error = fixture.pooled(&options).await.unwrap_err().to_string();
        assert!(error.contains("scenario case probe ran"), "{error}");
        assert!(error.contains("must_show"), "{error}");
        assert!(error.contains("must_not_show"), "{error}");
        assert!(error.contains("re-run the evaluation"), "{error}");
        let unrecorded = source(
            "unrecorded",
            json!({"id": "ran", "verdict": "present", "quote": null}),
        );
        let mut options = fixture.options(fixture.directory.join("pooled-again"));
        options.pool = vec![unrecorded];
        let error = fixture.pooled(&options).await.unwrap_err().to_string();
        assert!(error.contains("probe ran recorded no kind"), "{error}");
        assert!(error.contains("re-run the evaluation"), "{error}");
    }

    /// The judge's first answer leaves a probe without a verdict, and every
    /// answer after a re-ask is complete.
    fn missing_then_complete(text: &str) -> Option<Value> {
        text.contains("You check statements").then(|| {
            content(if text.contains("Your answer could not be used") {
                "[{\"id\": \"ran\", \"verdict\": \"present\", \"quote\": \"turn/end\"}, \
                 {\"id\": \"floods\", \"verdict\": \"absent\", \"quote\": \"nothing\"}]"
            } else {
                "[{\"id\": \"ran\", \"verdict\": \"present\", \"quote\": \"turn/end\"}]"
            })
        })
    }
    /// A reply with no verdict for a probe is asked again, and the request
    /// names the probe (issue #642, acceptance 2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_reply_with_no_verdict_for_a_probe_is_asked_again() {
        let fixture = Fixture::start(
            "missing",
            missing_then_complete,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [
                    {"id": "ran", "kind": "must_show", "text": "the run has a transcript"},
                    {"id": "floods", "kind": "must_not_show", "text": "the run floods the Timeline"}
                ]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["judgeReasks"], 1, "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
            assert_eq!(record["probes"][0]["verdict"], "present", "{record}");
            assert_eq!(record["probes"][1]["verdict"], "absent", "{record}");
        }
        let judge = fixture.judge();
        assert_eq!(judge.len(), 4, "one call and one re-ask per arm");
        for pair in judge.chunks(2) {
            assert!(
                pair[1].contains("Your answer could not be used: it has no verdict for floods."),
                "the re-ask names the probe: {}",
                pair[1]
            );
        }
    }

    /// The judge's call fails, the way a call refused for exceeding the
    /// judge's window does: the provider answers with an error instead of a
    /// reply.
    fn overflowing(text: &str) -> Option<Value> {
        text.contains("You check statements")
            .then(|| refusal("This model's maximum context length is 24000 tokens"))
    }
    /// The provider's refusal of one call.
    fn refusal(message: &str) -> Value {
        json!({"error": {
            "message": message,
            "type": "invalid_request_error",
            "code": "context_length_exceeded"
        }})
    }
    /// A judge call that fails leaves a failed run, not an unjudged one: it
    /// counts in failed runs, its probes are `unparsed`, and the report does
    /// not list it among the unjudged runs (model-prompts §8.1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_judge_call_that_fails_leaves_the_run_failed() {
        let fixture = Fixture::start(
            "call-failed",
            overflowing,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["failed"], true, "{record}");
            assert_eq!(record["judgeOutcome"], "call_failed", "{record}");
            assert_eq!(record["stop"], "idle", "{record}");
            assert_eq!(record["judgeReasks"], 0, "{record}");
            assert_eq!(record["probes"][0]["verdict"], "unparsed", "{record}");
            assert!(
                record["judgeError"]
                    .as_str()
                    .unwrap()
                    .starts_with("the judge failed: "),
                "{record}"
            );
        }
        assert_eq!(fixture.judge().len(), 4, "two calls per arm");
        assert!(
            report.contains("| judge temperature | dropped in 2 of 2 runs the judge was asked |"),
            "a run whose first call failed is a run the judge was asked: {report}"
        );
        assert!(
            report.contains("| failed runs | 1/1 (1.000) | 1/1 (1.000) |"),
            "{report}"
        );
        assert!(
            !report.contains("| arm | scenario | run | judge error |"),
            "a failed run is not listed as unjudged: {report}"
        );
    }

    /// The judge's first answer is unreadable and the call its re-ask makes
    /// fails: the run is a failed run with one re-ask recorded, and its error
    /// names the call that failed (model-prompts §8.1).
    fn prose_then_overflowing(text: &str) -> Option<Value> {
        text.contains("You check statements").then(|| {
            if text.contains("Your answer could not be used") {
                refusal("This model's maximum context length is 24000 tokens")
            } else {
                content("the run looks fine to me")
            }
        })
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_judge_call_that_fails_during_a_re_ask_leaves_the_run_failed() {
        let fixture = Fixture::start(
            "re-ask-failed",
            prose_then_overflowing,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["failed"], true, "{record}");
            assert_eq!(record["judgeOutcome"], "call_failed", "{record}");
            assert_eq!(record["judgeReasks"], 1, "{record}");
            assert_eq!(record["probes"][0]["verdict"], "unparsed", "{record}");
            assert!(
                record["judgeError"]
                    .as_str()
                    .unwrap()
                    .starts_with("the judge failed on a re-ask: "),
                "the error names the call that failed: {record}"
            );
        }
        assert!(
            report.contains("| failed runs | 1/1 (1.000) | 1/1 (1.000) |"),
            "{report}"
        );
        assert!(
            !report.contains("| arm | scenario | run | judge error |"),
            "a failed run is not listed as unjudged: {report}"
        );
    }

    /// The judge is named with the result, and one from the model's own
    /// family is reported as a limitation of the measurement.
    #[test]
    fn a_same_family_judge_is_reported_as_a_limitation() {
        let same = report(
            &instrument(ModelId {
                provider_id: "gateway".into(),
                model_id: "auto/deepseek-v4.1".into(),
            }),
            &[],
            &[],
        );
        assert!(same.contains("`gateway/auto/deepseek-v4.1`"), "{same}");
        assert!(same.contains("own family"), "{same}");
        let other = report(
            &instrument(ModelId {
                provider_id: "gateway".into(),
                model_id: "claude-opus-5-5".into(),
            }),
            &[],
            &[],
        );
        assert!(!other.contains("own family"), "{other}");
    }

    /// The candidate text every fixture's candidate arm carries.
    const CANDIDATE: &str = "CANDIDATE SUMMARY PROMPT. Summarize the transcript briefly.";
    /// The text the workspace file `notes.txt` holds; a run's request carries
    /// it only once the `read` tool has returned it.
    const FILE_TEXT: &str = "The notes file says the read tool carried this line back.";

    /// The judge's answer to a judge request, and `None` for any other.
    fn judged(text: &str) -> Option<Value> {
        text.contains("You check statements")
            .then(|| content(r#"[{"id":"ran","verdict":"present","quote":"turn/end"}]"#))
    }
    /// Every distinct subscription id a request carries, first seen first:
    /// the `events` tool's subscribe answers, in the order they arrived.
    fn subscription_ids(text: &str) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("sub_") {
            let after = &rest[start + "sub_".len()..];
            let end = after
                .find(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
                .unwrap_or(after.len());
            let id = format!("sub_{}", &after[..end]);
            if !ids.contains(&id) {
                ids.push(id);
            }
            rest = &after[end..];
        }
        ids
    }
    /// How many times one turn reads the workspace file before it answers.
    const READS: usize = 3;
    /// The fixture models' output allowance: their `maxOutputTokens` and the
    /// requests' `maxTokens`. The input budget is the window less this.
    const OUTPUT_TOKENS: u64 = 200;
    const VIEW_WINDOW: u64 = 32_000;
    /// The judge's answers, its refusal of a request its window cannot hold,
    /// the way a judge model refuses one over its window, and a `read` of the
    /// workspace file for a run request that has not read it [`READS`] times.
    fn reads_until_the_window_overflows(text: &str) -> Option<Value> {
        if text.contains("You check statements") {
            return (text.len() > VIEW_WINDOW as usize * 4)
                .then(|| {
                    refusal(&format!(
                        "This model's maximum context length is {VIEW_WINDOW} tokens"
                    ))
                })
                .or_else(|| judged(text));
        }
        (text.matches(FILE_TEXT).count() < READS)
            .then(|| tool_call("read", r#"{"path":"notes.txt"}"#))
    }
    /// The judge's verdict, the summarizer's two texts, and plain text for
    /// everything else.
    fn summaries_and_judgements(text: &str) -> Option<Value> {
        judged(text).or_else(|| {
            (text.contains("You summarize a coding session")
                || text.contains("CANDIDATE SUMMARY PROMPT"))
            .then(|| content("SUMMARY OF EARLIER TURNS"))
        })
    }
    /// The judge's verdict, the summarizer's text, and a `read` of the
    /// workspace's file for a run request that does not carry the file's
    /// text yet.
    fn reads_the_workspace_file(text: &str) -> Option<Value> {
        judged(text).or_else(|| {
            text.contains("You summarize a coding session")
                .then(|| content("SUMMARY OF EARLIER TURNS"))
                .or_else(|| {
                    (!text.contains(FILE_TEXT))
                        .then(|| tool_call("read", r#"{"path":"notes.txt"}"#))
                })
        })
    }

    /// One disposable product home, one scenario and one loopback provider:
    /// the fixture of the end-to-end runs. It stops the provider and removes
    /// its directory when it drops.
    struct Fixture {
        directory: PathBuf,
        provider: Arc<Provider>,
        http: tokio::task::JoinHandle<()>,
        models: PathBuf,
        scenarios: PathBuf,
        /// The process facts a flow's runs see: the ones this process has,
        /// with `home` pointed at the fixture's own empty directory, so a run
        /// never reads the developer's `~/AGENTS.md`, Skills list or `<home>`.
        platform: HostPlatform,
    }
    impl Fixture {
        /// `scenario` is the scenario file's content and `workspace` the
        /// starting files beside it; no answer is held.
        async fn start(
            label: &str,
            reply: Reply,
            scenario: Value,
            workspace: &[(&str, &str)],
        ) -> Self {
            Self::with_window(label, reply, scenario, workspace, 24_000).await
        }
        /// [`start`](Self::start) with the run model's declared window set to
        /// `window` instead of the fixture's own 24,000 tokens: a scenario
        /// sized from the compaction rule declares the window it derives.
        async fn with_window(
            label: &str,
            reply: Reply,
            scenario: Value,
            workspace: &[(&str, &str)],
            window: u64,
        ) -> Self {
            Self::holding(label, reply, holds_nothing, scenario, workspace, window).await
        }
        /// [`start`](Self::start) with a provider that holds the answers
        /// [`Hold`] names before it sends them.
        async fn holding(
            label: &str,
            reply: Reply,
            hold: Hold,
            scenario: Value,
            workspace: &[(&str, &str)],
            window: u64,
        ) -> Self {
            let provider = Arc::new(Provider {
                requests: Mutex::default(),
                reply,
                hold,
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = listener.local_addr().unwrap();
            let app = axum::Router::new()
                .route("/v1/chat/completions", axum::routing::post(completion))
                .with_state(provider.clone());
            let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let directory = std::env::temp_dir().join(format!("dim-eval-{label}"));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).unwrap();
            let models = directory.join("models.json");
            std::fs::write(
                &models,
                serde_json::to_string_pretty(&json!({
                    "defaultModel": {"providerId": "gateway", "modelId": "mock"},
                    "providers": [
                        {
                            "id": "gateway",
                            "kind": "openai-chat",
                            "baseUrl": format!("http://{endpoint}/v1"),
                            "models": [{
                                "id": "mock",
                                "contextWindow": window,
                                "maxOutputTokens": OUTPUT_TOKENS,
                                "config": {"maxTokens": OUTPUT_TOKENS}
                            }]
                        },
                        // The same upstream model under its own provider, with
                        // the wide window a flow judges views that outgrow the
                        // run model's (the module doc's two-entry
                        // configuration).
                        {
                            "id": "judge",
                            "kind": "openai-chat",
                            "baseUrl": format!("http://{endpoint}/v1"),
                            "models": [{
                                "id": "mock",
                                "contextWindow": 128000,
                                "maxOutputTokens": OUTPUT_TOKENS,
                                "config": {"maxTokens": OUTPUT_TOKENS}
                            }]
                        }
                    ]
                }))
                .unwrap(),
            )
            .unwrap();
            let scenarios = directory.join("scenarios");
            let case = scenarios.join("case");
            std::fs::create_dir_all(case.join("workspace")).unwrap();
            for (name, text) in workspace {
                std::fs::write(case.join("workspace").join(name), text).unwrap();
            }
            std::fs::write(
                case.join("scenario.json"),
                serde_json::to_string_pretty(&scenario).unwrap(),
            )
            .unwrap();
            let platform_home = directory.join("platform-home");
            std::fs::create_dir_all(&platform_home).unwrap();
            let env: Vec<_> = std::env::vars_os().collect();
            let shell = resolve_shell(Ok(None), &env).await;
            let mut platform = HostPlatform::current(shell)
                .unwrap()
                .with_execution_settings(
                    dim_agent_capabilities::execution_settings::ExecutionSettingsHost::fixed(
                        Default::default(),
                    ),
                );
            platform.home = platform_home;
            Self {
                directory,
                provider,
                http,
                models,
                scenarios,
                platform,
            }
        }
        /// [`evaluate`] over this fixture's runs, whose process facts point at
        /// the fixture's empty home: a flow that sizes a view or compacts a
        /// context would otherwise measure the developer's machine (issue
        /// #635).
        async fn evaluate(&self, options: &Options) -> Result<String> {
            evaluate(options).await
        }
        /// [`rejudge`] over this fixture's provider, with the same process
        /// facts as the re-judge flows served them before this work.
        async fn rejudge(&self, options: &Options) -> Result<String> {
            rejudge(
                options,
                options.rejudge.as_deref().expect("a rejudge source"),
                |builder| {
                    builder.process_value(Arc::new(self.platform.clone()));
                },
            )
            .await
        }
        /// [`pooled`] over this fixture's provider, with the same process
        /// facts as [`evaluate`](Self::evaluate).
        async fn pooled(&self, options: &Options) -> Result<String> {
            pooled(options, |builder| {
                builder.process_value(Arc::new(self.platform.clone()));
            })
            .await
        }
        /// The options of one run per arm over this fixture, with `out`.
        fn options(&self, out: PathBuf) -> Options {
            Options {
                scenarios: self.scenarios.clone(),
                texts: pairs(&[("context-compaction/summary", CANDIDATE)]),
                model: self.target(),
                judge: self.target(),
                runs: 1,
                jobs: 1,
                out,
                model_config: Some(self.models.clone()),
                isolate: true,
                fail_preflight: false,
                offer_plugin: false,
                extra_tool: false,
                fail_child: None,
                fail_child_holds_stdout: false,
                defaults: None,
                node: None,
                js_runtime: None,
                rejudge: None,
                select: Vec::new(),
                pool: Vec::new(),
            }
        }
        /// [`options`](Self::options) with the judge under the provider entry
        /// whose window holds a view that outgrows the run model's.
        fn judge_options(&self, out: PathBuf) -> Options {
            let mut options = self.options(out);
            options.judge = ModelId {
                provider_id: "judge".into(),
                model_id: "mock".into(),
            };
            options
        }
        fn target(&self) -> ModelId {
            ModelId {
                provider_id: "gateway".into(),
                model_id: "mock".into(),
            }
        }
        /// The requests one arm made of its own run, in arrival order: each
        /// carries the run's directory, `<out>/<arm>/<scenario>/run-<n>`, and
        /// the judge's requests are left out.
        fn arm(&self, arm: &str) -> Vec<String> {
            self.provider
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.contains(&format!("/{arm}/")))
                .filter(|request| !request.contains("You check statements"))
                .cloned()
                .collect()
        }
        /// The judge requests the provider received, in arrival order.
        fn judge(&self) -> Vec<String> {
            self.provider
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.contains("You check statements"))
                .cloned()
                .collect()
        }
        /// The records the evaluation wrote under `out`.
        fn records(&self, out: &Path) -> Vec<Value> {
            std::fs::read_to_string(out.join("runs.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
        /// [`options`](Self::options) with the product's plugins served from
        /// `defaults`, the way the runner's own command line serves them.
        fn plugin_options(&self, out: PathBuf, defaults: PathBuf) -> Options {
            let mut options = self.options(out);
            options.defaults = Some(defaults);
            options.node = Some(
                std::env::var_os("DIM_TEST_NODE")
                    .expect("DIM_TEST_NODE")
                    .into(),
            );
            options.js_runtime = Some(
                std::env::var_os("DIM_TEST_JS_RUNTIME")
                    .expect("DIM_TEST_JS_RUNTIME")
                    .into(),
            );
            options
        }
    }

    /// The repository root, whose default plugin configuration and fixtures
    /// the tests reach.
    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .unwrap()
    }

    /// One arm's run of the fixture's one scenario, as [`evaluate`] runs it.
    async fn run_arm(
        fixture: &Fixture,
        options: &Options,
        arm: &'static str,
        overrides: &Overrides,
        run: usize,
    ) -> RunRecord {
        let mut scenarios = read_scenarios(&fixture.scenarios).unwrap();
        let scenario = scenarios.remove(0);
        let mut instrument = Instrument::new(options);
        let cancel = CancellationToken::new();
        let native = native_probe(options, &cancel).await.unwrap();
        run_once(
            options,
            arm,
            overrides,
            &scenario,
            run,
            &cancel,
            &native,
            &mut instrument,
        )
        .await
        .unwrap()
    }

    /// A scenario that sets a goal through `calls` before its message.
    fn goal_call_scenario() -> Value {
        json!({
            "message": "Continue the work.",
            "calls": [{"method": "session.goal.set", "params": {"objective": "Keep the suite green."}}],
            "budget": {"turns": 6, "seconds": 60, "window": 24000},
            "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
        })
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.http.abort();
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// A message whose serialized size is `bytes`: the product counts a
    /// request's messages by their serialized JSON at four bytes per token
    /// (`crates/dim-agent-capabilities/src/context_model.rs`, the window
    /// facts' `BYTES_PER_TOKEN`), and a message's own envelope takes the rest.
    fn compaction_message(bytes: usize) -> String {
        let opening = "first ";
        let envelope = serde_json::to_vec(&Message::text(Role::User, ""))
            .unwrap()
            .len();
        format!("{opening}{}", "x".repeat(bytes - envelope - opening.len()))
    }

    /// The fixed request the fixture's runs build, in tokens: the system
    /// prompt, the tool declarations and the delivered context, which every
    /// request of the scenario carries, without the scenario's own message.
    /// One run of the same assembly with a small message measures it the way
    /// the product counts a request: its first `context/budget` estimate less
    /// the probe message's own tokens at four bytes per token.
    async fn fixed_request() -> f64 {
        const PROBE_WINDOW: u64 = 128_000;
        const PROBE_MESSAGE: &str = "Measure the fixed request.";
        let fixture = Fixture::with_window(
            "fixed-request",
            judged,
            json!({
                "message": PROBE_MESSAGE,
                "budget": {"turns": 1, "seconds": 60, "window": PROBE_WINDOW},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
            PROBE_WINDOW,
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let record = run_arm(&fixture, &options, "current", &Overrides::default(), 0).await;
        assert_eq!(
            record.error,
            None,
            "the probe run finishes its turn: {}",
            serde_json::to_string(&record).unwrap()
        );
        let export = std::fs::read_to_string(
            options
                .out
                .join("outputs")
                .join("current")
                .join("case")
                .join("run-0.timeline.jsonl"),
        )
        .unwrap();
        let (mut estimate, mut message_tokens, mut prepared) = (None, 0., false);
        for line in export.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let event = &value["event"];
            match (event["type"].as_str(), event["name"].as_str()) {
                (Some("context"), Some("prepare")) if !prepared => {
                    prepared = true;
                    for message in event["data"]["request"]["messages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        let message: Message = serde_json::from_value(message.clone()).unwrap();
                        let json = serde_json::to_vec(&message).unwrap();
                        if String::from_utf8_lossy(&json).contains(PROBE_MESSAGE) {
                            message_tokens += json.len() as f64 / 4.;
                        }
                    }
                }
                (Some("context"), Some("budget")) => {
                    estimate = event["data"]["estimatedInputTokens"].as_f64();
                }
                _ => {}
            }
        }
        estimate.expect("the probe run writes its context/budget estimate") - message_tokens
    }

    /// The relative path from the process's working directory to `target`:
    /// one `..` per component of the working directory, then the target's own
    /// components after its root.
    fn relative_to_working_directory(target: &Path) -> PathBuf {
        let mut path = PathBuf::new();
        for _ in std::env::current_dir().unwrap().components().skip(1) {
            path.push("..");
        }
        path.extend(target.components().skip(1));
        path
    }

    /// Both arms run the shipped assembly on the same scenario, and the
    /// candidate text reaches the candidate arm's summarizer request and only
    /// that arm's. The judge is the wide one: the run's message makes its view
    /// outgrow the run model's window.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn only_the_candidate_arm_carries_the_candidate_text() {
        // The scenario makes the first turn's request cross the compaction
        // threshold, and its cut replaces this run's whole history, so each
        // arm's summarizer sees the message and the request after the change
        // carries the compacted list. The sizes follow the compaction rule
        // (`crates/dim-agent-capabilities/src/context_model.rs` and
        // `context_policy.rs`) and the fixed request the assembly builds here,
        // so a longer system prompt or tool catalog moves them with it.
        //
        // With B the input budget, the window less the output allowance, the
        // trigger fires at B less its reserve: 10% of B, at least 16,000
        // tokens and at most 20% of it, so at 0.8B while that cap binds
        // (B under 80,000). A cut fits while its resident parts stay under
        // 0.5B: the request's frame and baseline, the digest at 0.05B, the
        // running turn's pinned input at 0.1B, and the replacement, a tenth
        // of B at most, capped at half the output allowance.
        //
        // B at four times the fixed request leaves 0.35B = 1.4 fixed for the
        // fixed request and its replacement, and a message of 2.5 fixed makes
        // the first request 3.5 fixed: over the 3.2 fixed threshold, under
        // the 4 fixed budget, and its cut's resident parts under half of it.
        let fixed = fixed_request().await;
        let window = (4. * fixed).ceil() as u64 + OUTPUT_TOKENS;
        let message = compaction_message((10. * fixed) as usize);
        let fixture = Fixture::with_window(
            "candidate",
            summaries_and_judgements,
            json!({
                "message": message,
                "steers": [{"afterSeconds": 1.0, "text": "second"}],
                "budget": {"turns": 6, "seconds": 120, "window": window},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
            window,
        )
        .await;
        // Two pairs, both in effect: the summary prompt and the branch
        // tool's description.
        const BRANCH: &str = "CANDIDATE BRANCH TOOL DESCRIPTION";
        let mut options = fixture.judge_options(fixture.directory.join("out"));
        options.texts = pairs(&[
            ("context-compaction/summary", CANDIDATE),
            ("timeline-tree/branch-description", BRANCH),
        ]);
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            // A steer taken inside the first turn joins it and starts no turn
            // of its own; one taken after the turn ends starts the second
            // turn. The scenario format's trigger is a tool call and this run
            // makes none, so the steer stays on the wall clock and either
            // shape may come out.
            let turns = record["turns"].as_u64().unwrap_or_default();
            assert!((1..=2).contains(&turns), "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
        }
        let current = fixture.arm("current");
        let candidate = fixture.arm("candidate");
        assert!(
            current
                .iter()
                .any(|request| request.contains("You summarize a coding session")),
            "the current arm's summarizer request carries the product prompt"
        );
        assert!(
            current
                .iter()
                .all(|request| !request.contains("CANDIDATE SUMMARY PROMPT")),
            "the current arm never carries the candidate text"
        );
        assert!(
            candidate
                .iter()
                .any(|request| request.contains("CANDIDATE SUMMARY PROMPT")),
            "the candidate arm's summarizer request carries the candidate text"
        );
        assert!(
            candidate
                .iter()
                .all(|request| !request.contains("You summarize a coding session")),
            "the candidate arm never carries the product prompt"
        );
        assert!(
            candidate.iter().any(|request| request.contains(BRANCH)),
            "the second pair's text reaches the candidate arm"
        );
        assert!(
            current.iter().all(|request| !request.contains(BRANCH)),
            "the current arm never carries the second pair's text"
        );
        // The compaction changes this run's history: the request right after
        // the change carries the whole message list, not the messages added
        // since.
        for arm in ["current", "candidate"] {
            let directory = options.out.join("outputs").join(arm).join("case");
            assert_view_rebuilds(&directory);
            let export = std::fs::read_to_string(directory.join("run-0.timeline.jsonl")).unwrap();
            assert_eq!(
                submitted_mode(&export, "second"),
                Some(json!("steer")),
                "{arm}: the timed steer's input/message carries mode steer"
            );
            let view = std::fs::read_to_string(directory.join("run-0.judge-view.jsonl")).unwrap();
            let changed = after_history_change(&export);
            let rendered: Value = serde_json::from_str(view.lines().nth(changed).unwrap()).unwrap();
            assert!(
                rendered["event"]["data"]["request"]["messages"].is_array(),
                "{arm}: the request after the history change carries the whole list: {rendered}"
            );
        }
    }

    /// A run whose export is longer than the judge's window is judged through
    /// the view, which carries each request's unchanged fields once: the judge
    /// refuses the raw export and answers the view (issue #635). The raw
    /// export alone is longer than the judge's window; the view fits it.
    ///
    /// One turn asks the model [`READS`] times with the file's text carried
    /// back each time, so the export carries the whole system prompt and tool
    /// catalog once per request while the view carries them once. Every
    /// request of the view is rebuilt from it and compared with the export's,
    /// so the view loses no field this run produces.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_run_longer_than_the_judge_window_is_judged_through_its_view() {
        let fixture = Fixture::with_window(
            "judge-view",
            reads_until_the_window_overflows,
            json!({
                "message": "Read notes.txt until you are sure of it.",
                "budget": {"turns": 4, "seconds": 60, "window": VIEW_WINDOW},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[("notes.txt", FILE_TEXT)],
            VIEW_WINDOW,
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
            assert_eq!(record["probes"][0]["verdict"], "present", "{record}");
        }
        let estimate = |text: &str| {
            let judge = ModelSelection {
                provider_id: "gateway".into(),
                model_id: "mock".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            };
            let request = judge_request(
                &judge,
                &[Message::text(Role::User, text)],
                &ModelConfig::default(),
            );
            window_facts(VIEW_WINDOW, 200., &request).count_tokens(&request)
        };
        for arm in ["current", "candidate"] {
            let directory = options.out.join("outputs").join(arm).join("case");
            let export = std::fs::read_to_string(directory.join("run-0.timeline.jsonl")).unwrap();
            let view = std::fs::read_to_string(directory.join("run-0.judge-view.jsonl")).unwrap();
            assert!(
                estimate(&export) > VIEW_WINDOW as f64,
                "{arm}: the raw export is longer than the judge's window"
            );
            assert!(
                estimate(&view) < (VIEW_WINDOW - OUTPUT_TOKENS) as f64,
                "{arm}: the view fits the judge's window"
            );
            // Every line survives in order and every request is rebuilt from
            // the view into the one the export carried.
            let requests = assert_view_rebuilds(&directory);
            assert!(
                requests.len() > 1,
                "{arm}: the run made more than one request"
            );
            assert!(
                requests.iter().any(|request| request["addedMessages"]
                    .as_array()
                    .is_some_and(|added| !added.is_empty())),
                "{arm}: a later request carries the messages added since"
            );
            for field in ["system", "tools", "config"] {
                assert!(
                    requests
                        .iter()
                        .any(|request| request[field] == json!({"unchanged": true})),
                    "{arm}: an unchanged {field} reads as the marker"
                );
            }
            assert!(
                requests[0]["system"] != json!({"unchanged": true}),
                "{arm}: the first request of a line is given in full"
            );
        }
    }

    /// Asserts that a run's `judge-view.jsonl` is its export line for line,
    /// with every request rebuilt from the view alone into the one the export
    /// carried, and answers the view's rendered requests in order.
    fn assert_view_rebuilds(directory: &Path) -> Vec<Value> {
        let export = std::fs::read_to_string(directory.join("run-0.timeline.jsonl")).unwrap();
        let view = std::fs::read_to_string(directory.join("run-0.judge-view.jsonl")).unwrap();
        assert_eq!(
            view.lines().count(),
            export.lines().count(),
            "the view keeps every line of the export"
        );
        let rebuilt = rebuilt_requests(&view);
        assert!(!rebuilt.is_empty(), "the export carries requests");
        for (index, request) in &rebuilt {
            let original: Value =
                serde_json::from_str(export.lines().nth(*index).unwrap()).unwrap();
            assert_eq!(
                *request, original["event"]["data"]["request"],
                "the request at line {index} is rebuilt from the view"
            );
        }
        view.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|line| {
                line["event"]["data"]["request"]
                    .is_object()
                    .then(|| line["event"]["data"]["request"].clone())
            })
            .collect()
    }

    /// The mode `session.submit` carried for the `input/message` event in
    /// `export` whose user text is `text`
    fn submitted_mode(export: &str, text: &str) -> Option<Value> {
        export
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|line| {
                line["event"]["type"] == "input"
                    && line["event"]["name"] == "message"
                    && line["event"]["data"]["user"]["content"]
                        .as_array()
                        .is_some_and(|blocks| {
                            blocks
                                .iter()
                                .any(|block| block["text"].as_str() == Some(text))
                        })
            })
            .map(|line| line["event"]["data"]["mode"].clone())
    }

    /// Whether any string in a request body carries `needle`: a request is
    /// JSON, so a text with quotes in it is compared through the parsed
    /// value, never through the escaped bytes.
    fn carried(request: &str, needle: &str) -> bool {
        fn holds(value: &Value, needle: &str) -> bool {
            match value {
                Value::String(text) => text.contains(needle),
                Value::Array(items) => items.iter().any(|item| holds(item, needle)),
                Value::Object(map) => map.values().any(|item| holds(item, needle)),
                _ => false,
            }
        }
        serde_json::from_str::<Value>(request).is_ok_and(|value| holds(&value, needle))
    }

    /// The line of a run's export holding the first request whose messages do
    /// not extend its line's previous request's: the request a history change
    /// produced.
    fn after_history_change(export: &str) -> usize {
        let mut previous: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for (index, raw) in export.lines().enumerate() {
            let Ok(line) = serde_json::from_str::<Value>(raw) else {
                continue;
            };
            let Some(messages) = line["event"]["data"]["request"]["messages"].as_array() else {
                continue;
            };
            let branch = line["branch_id"].as_str().unwrap_or_default().to_owned();
            if let Some(earlier) = previous.get(&branch) {
                let extends =
                    messages.len() >= earlier.len() && messages[..earlier.len()] == earlier[..];
                if !extends {
                    return index;
                }
            }
            previous.insert(branch, messages.clone());
        }
        panic!("the run's export holds a request after a history change");
    }

    /// A run whose view does not fit the judge's budget is unjudged before
    /// any call — the judge is never asked, and the report lists the run with
    /// the estimate and the budget — while a run whose view fits is judged
    /// (issue #635).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_run_whose_view_does_not_fit_the_judge_is_unjudged() {
        let fixture = Fixture::start(
            "judge-fit",
            judged,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        // The second scenario's message makes its view exceed the judge's
        // budget, which is the host model's window; the first scenario's view
        // fits it.
        let over = fixture.scenarios.join("over");
        std::fs::create_dir_all(over.join("workspace")).unwrap();
        std::fs::write(
            over.join("scenario.json"),
            serde_json::to_string_pretty(&json!({
                "message": format!("Say hello. {}", "x".repeat(20_000)),
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }))
            .unwrap(),
        )
        .unwrap();
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 4, "one run per arm and scenario");
        for record in &records {
            assert_eq!(record["failed"], false, "{record}");
            if record["scenario"] == "case" {
                assert_eq!(record["judgeOutcome"], "judged", "{record}");
                assert_eq!(record["probes"][0]["verdict"], "present", "{record}");
            } else {
                assert_eq!(record["judgeOutcome"], "unfit", "{record}");
                assert_eq!(record["probes"][0]["verdict"], "unparsed", "{record}");
                let error = record["judgeError"].as_str().unwrap();
                assert!(error.starts_with("the judge view needs about "), "{error}");
                assert!(error.contains(" tokens; the judge's budget is "), "{error}");
            }
        }
        assert_eq!(
            fixture.judge().len(),
            2,
            "the judge is asked once for each run whose view fits, and never for the other"
        );
        assert!(
            report.contains("| unjudged runs | 1/2 (0.500) | 1/2 (0.500) |"),
            "the unjudged-run rate compares the runs the judge was not asked about: {report}"
        );
        assert!(
            report.contains("| current | over | 0 | the judge view needs about "),
            "the report lists the unjudged run with its estimate and budget: {report}"
        );
        assert!(
            report.contains("| candidate | over | 0 | the judge view needs about "),
            "{report}"
        );
        assert!(
            report.contains("| case/ran (must_show) | 1/1 (1.000) | 1/1 (1.000) |"),
            "the runs whose view fits are judged: {report}"
        );
        assert!(
            report.contains("| over/ran (must_show) | – | – |"),
            "an unjudged run is out of the probe rates: {report}"
        );
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
    }

    /// A run offers the shipped assembly's whole tool catalog — the workspace
    /// tools and Questions, not only the mock profile's three — carries a
    /// workspace tool's result into its next request (model-prompts §8.1), and
    /// fires an operator steer on that tool's call, whose text reaches the
    /// model with the result.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_run_offers_the_product_tools_and_carries_a_tool_result() {
        const STEER: &str = "STEER: name the file you just read.";
        let fixture = Fixture::start(
            "tools",
            reads_the_workspace_file,
            json!({
                "message": "Read notes.txt in the workspace and tell me what it says.",
                "steers": [{"whenToolCall": "read", "text": STEER}],
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            // A steer taken before the next request is assembled joins it
            // (one turn, two requests); one taken later replaces a request in
            // flight or starts a turn of its own (three requests, one or two
            // turns).
            let turns = record["turns"].as_u64().unwrap_or_default();
            assert!((1..=2).contains(&turns), "{record}");
        }
        for arm in ["current", "candidate"] {
            let export = std::fs::read_to_string(
                options
                    .out
                    .join("outputs")
                    .join(arm)
                    .join("case")
                    .join("run-0.timeline.jsonl"),
            )
            .unwrap();
            assert_eq!(
                submitted_mode(&export, STEER),
                Some(json!("steer")),
                "{arm}: the event-triggered steer's input/message carries mode steer"
            );
            let requests = fixture.arm(arm);
            assert!(
                (2..=3).contains(&requests.len()),
                "{arm}: {} requests: {requests:?}",
                requests.len()
            );
            let body: Value = serde_json::from_str(&requests[0]).unwrap();
            let offered: Vec<&str> = body["tools"]
                .as_array()
                .map(|tools| {
                    tools
                        .iter()
                        .filter_map(|tool| tool["function"]["name"].as_str())
                        .collect()
                })
                .unwrap_or_default();
            // The run's mount list: the product's whole tool set without
            // the `plugin` tool, in mount order. This pin catches a
            // product tool removal (the registry drops a mounted name it
            // cannot resolve) and any divergence in the mounted order; a
            // product tool addition is the `tools-product` preflight
            // check's catch, at run time.
            assert_eq!(
                offered, RUN_TOOLS,
                "{arm}: the run Session offers exactly the run's tool set"
            );
            assert!(
                requests[1].contains(FILE_TEXT),
                "{arm}: the read result reaches the next request"
            );
            let last = requests.last().expect("a request");
            assert!(
                last.contains(STEER),
                "{arm}: the steer's text reaches a request"
            );
        }
    }

    /// A relative `--out` is created and resolved at the command line, so the
    /// Session's cwd is absolute and both arms run: `session.create` refuses
    /// a relative cwd. The relative path is the fixture's own output
    /// directory, spelled from the working directory the test process runs
    /// in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_relative_out_resolves_before_the_run_uses_it_as_its_cwd() {
        let fixture = Fixture::start(
            "relative",
            judged,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let candidate = fixture.directory.join("candidate.md");
        std::fs::write(&candidate, CANDIDATE).unwrap();
        let out = fixture.directory.join("out");
        let relative = relative_to_working_directory(&out);
        let mut options = Options::parse_from(
            [
                "--scenarios",
                fixture.scenarios.to_str().unwrap(),
                "--text",
                "context-compaction/summary",
                "--candidate",
                candidate.to_str().unwrap(),
                "--model",
                "gateway/mock",
                "--judge",
                "gateway/mock",
                "--runs",
                "1",
                "--model-config",
                fixture.models.to_str().unwrap(),
                "--out",
                relative.to_str().unwrap(),
            ]
            .map(str::to_owned),
        )
        .unwrap();
        // This test is about the command line's paths; the product's
        // plugins are the command line's other half and need Node.
        options.defaults = None;
        options.node = None;
        options.js_runtime = None;
        assert!(
            options.out.is_absolute(),
            "a relative --out is resolved: {}",
            options.out.display()
        );
        assert_eq!(
            options.out,
            out.canonicalize().unwrap(),
            "the resolved --out is the fixture's output directory"
        );
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}"
        );
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
        }
        assert!(options.out.join("current/case/run-0/workspace").is_dir());
    }

    /// A line that stays busy across many checks is asked once: its request
    /// is still outstanding, so no check sends a second one.
    #[test]
    fn a_busy_line_keeps_one_idle_request_outstanding() {
        let mut calls = IdleCalls::default();
        let mut sent = 0;
        for _ in 0..64 {
            if calls.outstanding("main").is_none() {
                sent += 1;
                calls.sent("main", sent);
            }
        }
        assert_eq!(sent, 1, "one request for every check the line stayed busy");
        assert_eq!(calls.outstanding("main"), Some(1));
    }

    /// A reply below an event the driver has read is stale: the request is
    /// spent, and the next check sends one new request.
    #[test]
    fn a_reply_below_the_latest_event_is_stale() {
        let mut calls = IdleCalls::default();
        calls.read("main", 7);
        calls.sent("main", 1);
        assert_eq!(calls.reply("main", Ok(json!({"sequence": 6}))), Ok(false));
        assert_eq!(calls.outstanding("main"), None);
        calls.sent("main", 2);
        assert_eq!(
            calls.outstanding("main"),
            Some(2),
            "the next check has one new request to wait on"
        );
    }

    /// A reply at the line's latest event counts as idle: nothing happened
    /// after the line's idle point.
    #[test]
    fn a_current_reply_counts_as_idle() {
        let mut calls = IdleCalls::default();
        calls.read("main", 7);
        calls.sent("main", 1);
        assert_eq!(calls.reply("main", Ok(json!({"sequence": 7}))), Ok(true));
        assert_eq!(calls.outstanding("main"), None);
    }

    /// An ended line answers `session.idle` with the `branch_ended` code, and
    /// it counts as idle: no turn is left to wait for.
    #[test]
    fn the_branch_ended_code_counts_as_ended() {
        let mut calls = IdleCalls::default();
        calls.read("main", 7);
        calls.sent("main", 1);
        let ended = Box::new(dim_client::RpcError {
            code: -32000,
            message: "main has ended".to_owned(),
            data: Some(json!({"appError": "branch_ended"})),
        });
        assert_eq!(calls.reply("main", Err(ended)), Ok(true));
        assert_eq!(calls.outstanding("main"), None);
    }

    /// `connection_busy` and every other refusal end the run: a busy line is
    /// not an ended one, and only the ended code may close a run early.
    #[test]
    fn connection_busy_and_any_other_code_is_an_error() {
        for code in ["connection_busy", "service_reloading"] {
            let mut calls = IdleCalls::default();
            calls.sent("main", 1);
            let refused = Box::new(dim_client::RpcError {
                code: -32000,
                message: "Connection request limit reached".to_owned(),
                data: Some(json!({"appError": code})),
            });
            let error = calls.reply("main", Err(refused)).expect_err(code);
            assert!(error.contains(code), "{error}");
            assert_eq!(
                calls.outstanding("main"),
                None,
                "the request is spent even when it fails the run"
            );
        }
    }

    /// A successful reply that carries no sequence proves nothing about the
    /// line's idle point, so it fails the run instead of counting as idle.
    #[test]
    fn a_reply_without_a_sequence_fails_the_run() {
        let mut calls = IdleCalls::default();
        calls.sent("main", 1);
        let error = calls
            .reply("main", Ok(json!({})))
            .expect_err("a reply with no sequence");
        assert!(error.contains("no sequence"), "{error}");
    }

    /// What a scripted App Server peer plays for `session.question.respond`.
    #[cfg(unix)]
    enum PeerScript {
        /// One response body per decline, in order, then `{}`.
        Replies(Vec<Value>),
        /// The Session registers the call's waiter after the client's first
        /// decline, and the line stays busy meanwhile: the peer streams a
        /// burst of unrelated notices, refuses every decline for the next
        /// half quiet period, takes the one after that, and settles the call
        /// with its `step/tool-result`.
        Late,
        /// The Session never takes a decline: the peer refuses every one and
        /// settles the call with its `step/tool-result` after the second, so
        /// a decline after that result can only be one the runner kept past
        /// it.
        Refused,
        /// A later request reuses the settled call id: the peer refuses the
        /// first decline, asks `ask_user` again as `c1` under `r2`, refuses
        /// that one too, settles only `r1`, and then takes the decline the
        /// runner sends for `r2` and settles that batch.
        ReusedCallId,
        /// The `step/tool-call` carries no `request_id`, which the event
        /// contract never does, so its call cannot be keyed.
        Unkeyed,
        /// The `ask_user` call carries no id, which `ToolCallData` never
        /// does, so its decline cannot be keyed to the call.
        CallWithoutId,
        /// Two `ask_user` calls in one batch, `c1` and `c2`: the peer refuses
        /// both at first, takes the first call's resend, and fails the
        /// second call's, so the run ends on that error after one resend.
        RetryFails,
        /// The line stays busy for [`HELD`] from the first `session.idle`
        /// call: the peer reads every call and answers none while it holds,
        /// then answers that one and every later one as idle at
        /// [`PEER_SEQUENCE`].
        HeldIdle,
    }

    /// What a scripted peer read: the `requestId` of every decline, in order,
    /// how many it had read when it first wrote a call's `step/tool-result`,
    /// and how many `session.idle` calls it read while it held the line busy.
    #[cfg(unix)]
    #[derive(Default)]
    struct PeerReads {
        declines: Mutex<Vec<String>>,
        settled_at: AtomicUsize,
        idles: AtomicUsize,
    }

    /// How long [`PeerScript::HeldIdle`] keeps the line busy: five quiet
    /// checks, one quiet period apart, the last one's deadline included.
    #[cfg(unix)]
    const HELD: Duration = Duration::from_secs(10);

    /// The sequence a scripted peer reports as its next dispatched one: above
    /// every event it writes, so its `session.idle` reply is never stale.
    #[cfg(unix)]
    const PEER_SEQUENCE: u64 = 1_000;

    /// A scripted App Server peer for the driver: it answers the Session
    /// methods the driver calls and plays the `ask_user` `step/tool-call`
    /// the script asks for after the submission. It keeps the line busy,
    /// answering no `session.idle`, until it writes a result that closes the
    /// call. It returns the socket and what it read.
    #[cfg(unix)]
    async fn scripted_peer(
        directory: &Path,
        window: u64,
        script: PeerScript,
    ) -> (PathBuf, Arc<PeerReads>) {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        /// The response frame for one request.
        fn frame(request: &Value, body: Value) -> Vec<u8> {
            let mut frame = json!({"jsonrpc": "2.0", "id": request["id"]});
            for (key, value) in body.as_object().into_iter().flatten() {
                frame[key] = value.clone();
            }
            format!("{frame}\n").into_bytes()
        }
        /// One `session.event` notice of `event`.
        fn notice(event: Value) -> Vec<u8> {
            let notice = json!({"jsonrpc": "2.0", "method": "session.event",
                                "params": {"sessionId": "peer", "branchId": "main",
                                           "event": event}});
            format!("{notice}\n").into_bytes()
        }
        /// The refusal a client gets while the call has no waiter.
        fn not_pending() -> Value {
            json!({"error": {"code": -32000, "message": "No ask_user call is waiting for this answer",
                             "data": {"appError": "question_not_pending"}}})
        }
        /// One `ask_user` `step/tool-call` at `sequence`, asking `calls` under
        /// `request`; a `None` request leaves the `request_id` out, and a
        /// `None` call leaves its id out.
        fn question_call(sequence: u64, request: Option<&str>, calls: &[Option<&str>]) -> Vec<u8> {
            let calls: Vec<Value> = calls
                .iter()
                .map(|call| {
                    let mut asked = json!({"name": "ask_user", "input":
                        {"questions": [{"header": "Deploy",
                                        "question": "Where to deploy?",
                                        "options": [{"label": "production"},
                                                    {"label": "staging"}]}]}});
                    if let Some(call) = call {
                        asked["id"] = json!(call);
                    }
                    asked
                })
                .collect();
            let mut data = json!({"turn_id": "t1", "calls": calls});
            if let Some(request) = request {
                data["request_id"] = json!(request);
            }
            notice(
                json!({"type": "step", "name": "tool-call", "id": format!("e{sequence}"),
                          "sequence": sequence, "data": data}),
            )
        }
        /// The `step/tool-result` that closes the call `question_call` sent at
        /// `sequence`, numbered two events on from it as a batch's call and
        /// result are.
        fn declined_result(sequence: u64, request: &str, call: &str) -> Vec<u8> {
            let result = sequence + 2;
            notice(
                json!({"type": "step", "name": "tool-result", "id": format!("e{result}"),
                          "sequence": result,
                          "data": {"turn_id": "t1", "request_id": request,
                                   "call_event_id": format!("e{sequence}"),
                                   "results": [{"call_id": call, "result": {
                                       "content": "User declined to answer. Proceed without these answers."}}]}}),
            )
        }

        let _ = std::fs::remove_dir_all(directory);
        private_directory(directory).unwrap();
        let socket = directory.join("peer.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let reads = Arc::new(PeerReads::default());
        let recorded = reads.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            let replies = match &script {
                PeerScript::Replies(replies) => replies.clone(),
                _ => Vec::new(),
            };
            let mut declines = 0;
            let mut first: Option<Instant> = None;
            // The line is busy until the peer writes the call's result, the
            // way a running turn is: `session.idle` stays unanswered and the
            // driver's own deadline reports the line busy.
            let mut settled = false;
            // [`PeerScript::HeldIdle`]'s hold: the calls it kept, and the
            // deadline it answers them at. The deadline stands after the
            // release, so a later call starts no second hold.
            let mut held: Vec<Value> = Vec::new();
            let mut hold_until: Option<Instant> = None;
            loop {
                let line = match hold_until {
                    Some(at) if !held.is_empty() => {
                        match tokio::time::timeout_at(at.into(), lines.next_line()).await {
                            Ok(line) => line,
                            // The hold ended: every call it kept is answered
                            // at the peer's next sequence.
                            Err(_) => {
                                for call in held.drain(..) {
                                    let body = json!({"result": {"sequence": PEER_SEQUENCE}});
                                    write.write_all(&frame(&call, body)).await.unwrap();
                                }
                                continue;
                            }
                        }
                    }
                    _ => lines.next_line().await,
                };
                let Some(line) = line.unwrap() else { break };
                let request: Value = serde_json::from_str(&line).unwrap();
                let method = request["method"].as_str().unwrap_or_default();
                if method == "session.idle" {
                    if matches!(script, PeerScript::HeldIdle) {
                        let at = *hold_until.get_or_insert_with(|| Instant::now() + HELD);
                        if Instant::now() < at {
                            recorded.idles.fetch_add(1, Ordering::SeqCst);
                            held.push(request);
                            continue;
                        }
                    } else if !settled {
                        continue;
                    }
                }
                if method == "session.question.respond" {
                    recorded.declines.lock().unwrap().push(
                        request["params"]["requestId"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    );
                    declines += 1;
                    // The batch whose `step/tool-result` the peer writes after
                    // answering this decline, and the sequence the batch's
                    // call went out at: a taken decline settles its own call,
                    // and a refused one only where the script has the Session
                    // close the call by other means.
                    let closed = match &script {
                        // No question is asked, so no decline arrives.
                        PeerScript::HeldIdle => None,
                        PeerScript::Replies(_) => {
                            let body = replies
                                .get(declines - 1)
                                .cloned()
                                .unwrap_or_else(|| json!({"result": {}}));
                            let taken = body.get("result").is_some();
                            write.write_all(&frame(&request, body)).await.unwrap();
                            taken.then_some(("r1", 1))
                        }
                        PeerScript::Late => {
                            let at = *first.get_or_insert_with(Instant::now);
                            let taken = at.elapsed() >= QUIET / 2;
                            let body = match taken {
                                true => json!({"result": {}}),
                                false => not_pending(),
                            };
                            write.write_all(&frame(&request, body)).await.unwrap();
                            if declines == 1 {
                                for line in 0..20 {
                                    let event = json!({"type": "step", "name": "text", "id": "b",
                                                       "data": {"turn_id": "t1", "request_id": "r1",
                                                                "text": format!("line {line}")}});
                                    write.write_all(&notice(event)).await.unwrap();
                                }
                            }
                            taken.then_some(("r1", 1))
                        }
                        PeerScript::Refused => {
                            write
                                .write_all(&frame(&request, not_pending()))
                                .await
                                .unwrap();
                            (declines == 2).then_some(("r1", 1))
                        }
                        PeerScript::ReusedCallId => match declines {
                            1 => {
                                write
                                    .write_all(&frame(&request, not_pending()))
                                    .await
                                    .unwrap();
                                write
                                    .write_all(&question_call(2, Some("r2"), &[Some("c1")]))
                                    .await
                                    .unwrap();
                                None
                            }
                            2 => {
                                write
                                    .write_all(&frame(&request, not_pending()))
                                    .await
                                    .unwrap();
                                Some(("r1", 1))
                            }
                            _ => {
                                write
                                    .write_all(&frame(&request, json!({"result": {}})))
                                    .await
                                    .unwrap();
                                Some(("r2", 2))
                            }
                        },
                        PeerScript::Unkeyed | PeerScript::CallWithoutId => {
                            write
                                .write_all(&frame(&request, not_pending()))
                                .await
                                .unwrap();
                            None
                        }
                        PeerScript::RetryFails => {
                            let body = match declines {
                                1 | 2 => not_pending(),
                                3 => json!({"result": {}}),
                                _ => json!({"error": {"code": -32000,
                                                      "message": "the Session is reloading",
                                                      "data": {"appError": "session_reloading"}}}),
                            };
                            write.write_all(&frame(&request, body)).await.unwrap();
                            (declines == 3).then_some(("r1", 1))
                        }
                    };
                    if let Some((request, sequence)) = closed {
                        if recorded.settled_at.load(Ordering::SeqCst) == 0 {
                            recorded.settled_at.store(declines, Ordering::SeqCst);
                        }
                        settled = true;
                        write
                            .write_all(&declined_result(sequence, request, "c1"))
                            .await
                            .unwrap();
                    }
                    continue;
                }
                let body = match method {
                    "session.create" => json!({"result": {"sessionId": "peer"}}),
                    "session.context.usage" => json!({"result": {"contextWindow": window}}),
                    "session.idle" => json!({"result": {"sequence": PEER_SEQUENCE}}),
                    _ => json!({"result": {}}),
                };
                write.write_all(&frame(&request, body)).await.unwrap();
                if method == "session.submit" {
                    write
                        .write_all(&notice(
                            json!({"type": "turn", "name": "start", "sequence": 1, "data": {}}),
                        ))
                        .await
                        .unwrap();
                    // The held line asks nothing: the run's only work is the
                    // turn the peer holds open.
                    if matches!(script, PeerScript::HeldIdle) {
                        continue;
                    }
                    let request = match &script {
                        PeerScript::Unkeyed => None,
                        _ => Some("r1"),
                    };
                    let calls = match &script {
                        PeerScript::CallWithoutId => vec![None],
                        PeerScript::RetryFails => vec![Some("c1"), Some("c2")],
                        _ => vec![Some("c1")],
                    };
                    write
                        .write_all(&question_call(1, request, &calls))
                        .await
                        .unwrap();
                }
            }
        });
        (socket, reads)
    }

    /// A client on a scripted peer's socket.
    #[cfg(unix)]
    async fn driver_client(socket: PathBuf) -> Client {
        Client::open(
            &ConnectionConfig {
                endpoint: socket,
                server: WireServer {
                    program: std::env::current_exe().unwrap(),
                    embedded: true,
                },
                profile: None,
                client_kind: "other",
                client_name: "scenario-eval",
                protocol_version: 4,
            },
            false,
        )
        .await
        .unwrap()
    }

    /// The model the driver names in `session.create` for a scripted peer.
    #[cfg(unix)]
    fn peer_model() -> ModelSelection {
        ModelSelection {
            provider_id: "gateway".into(),
            model_id: "mock".into(),
            capability_snapshot: None,
            config: None,
            metadata: None,
        }
    }

    /// Any other error from `session.question.respond` ends the run with that
    /// error, instead of being dropped and leaving the run on its budget.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn another_question_error_ends_the_run() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-scripted-{}", std::process::id()));
        let (socket, _) = scripted_peer(
            &directory,
            8_000,
            PeerScript::Replies(vec![
                json!({"error": {"code": -32000, "message": "the Session is reloading",
                                  "data": {"appError": "session_reloading"}}}),
            ]),
        )
        .await;
        let mut client = driver_client(socket).await;
        let mut scenario = scenario(Vec::new());
        scenario.budget.seconds = 10.;
        let driven = drive(
            &mut client,
            &scenario,
            &std::env::temp_dir(),
            &peer_model(),
            &ProducerState::default(),
            None,
        )
        .await;
        let _ = std::fs::remove_dir_all(directory);
        // The budget writes `seconds`, so `error` is the run ending on the
        // reply, not on its budget.
        assert_eq!(driven.stop, "error", "{:?}", driven.error);
        let error = driven.error.unwrap_or_default();
        assert!(
            error.contains("the decline of c1 failed") && error.contains("session_reloading"),
            "{error}"
        );
    }

    /// A busy line does not multiply the declines: the peer refuses the first
    /// decline and streams a burst of unrelated notices at once, and the
    /// runner answers the decline again only once the quiet period has
    /// passed. The peer reads exactly two declines, and the run ends idle
    /// because the retry took the second one.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_busy_line_sends_the_decline_again_once() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-busy-{}", std::process::id()));
        let (socket, reads) = scripted_peer(&directory, 8_000, PeerScript::Late).await;
        let mut client = driver_client(socket).await;
        let mut scenario = scenario(Vec::new());
        scenario.budget.seconds = 10.;
        let driven = drive(
            &mut client,
            &scenario,
            &std::env::temp_dir(),
            &peer_model(),
            &ProducerState::default(),
            None,
        )
        .await;
        let _ = std::fs::remove_dir_all(directory);
        // The budget writes `seconds`, so `idle` is the retry ending the run.
        assert_eq!(driven.stop, "idle", "{:?}", driven.error);
        assert_eq!(
            driven.decline_retries, 1,
            "the retry the quiet period scheduled"
        );
        assert_eq!(
            reads.declines.lock().unwrap().len(),
            2,
            "one declined early and one the quiet period scheduled"
        );
    }

    /// A kept decline ends at its call's result, not at the Session's answer:
    /// the peer refuses every decline and settles `c1` with its
    /// `step/tool-result`, and the runner reads the first decline and one
    /// retry and sends nothing after the result.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_kept_decline_is_dropped_at_its_calls_result() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-kept-{}", std::process::id()));
        let (socket, reads) = scripted_peer(&directory, 8_000, PeerScript::Refused).await;
        let mut client = driver_client(socket).await;
        let mut scenario = scenario(Vec::new());
        scenario.budget.seconds = 10.;
        let driven = drive(
            &mut client,
            &scenario,
            &std::env::temp_dir(),
            &peer_model(),
            &ProducerState::default(),
            None,
        )
        .await;
        // The peer reads a request only after the driver sent it, so the
        // counts are final once it has read the last one.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = std::fs::remove_dir_all(directory);
        let declines = reads.declines.lock().unwrap().len();
        let settled_at = reads.settled_at.load(Ordering::SeqCst);
        // The budget writes `seconds`, so `idle` is the result closing the
        // call and the line.
        assert_eq!(driven.stop, "idle", "{:?}", driven.error);
        assert!(
            settled_at >= 2,
            "the peer reads the first decline and a retry: {declines} declines"
        );
        assert_eq!(
            declines, settled_at,
            "no decline arrives after the call's result: {declines} declines, {settled_at} before the result"
        );
    }

    /// A later request reusing the settled call id is not closed by the stale
    /// entry's result: the key holds the `requestId` as well as the call id,
    /// and the later call's own decline is sent (Questions §4.3).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_later_request_reusing_the_call_id_keeps_its_own_decline() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-reused-{}", std::process::id()));
        let (socket, reads) = scripted_peer(&directory, 8_000, PeerScript::ReusedCallId).await;
        let mut client = driver_client(socket).await;
        let mut scenario = scenario(Vec::new());
        scenario.budget.seconds = 10.;
        let driven = drive(
            &mut client,
            &scenario,
            &std::env::temp_dir(),
            &peer_model(),
            &ProducerState::default(),
            None,
        )
        .await;
        // The peer reads a request only after the driver sent it, so the
        // counts are final once it has read the last one.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = std::fs::remove_dir_all(directory);
        let declines = reads.declines.lock().unwrap().clone();
        let settled_at = reads.settled_at.load(Ordering::SeqCst);
        assert_eq!(driven.stop, "idle", "{:?}", driven.error);
        assert_eq!(
            settled_at, 2,
            "the stale batch's result went out after two declines: {declines:?}"
        );
        assert_eq!(
            declines,
            ["r1", "r2", "r2"],
            "the later call's decline is sent after the stale batch settled"
        );
    }

    /// A question's `step/tool-call` always carries its batch's `request_id`
    /// (`ToolCallData`), so an `ask_user` call without one cannot be keyed to
    /// any reply: the run ends with an error naming the call rather than
    /// keeping a decline that would be sent again for the rest of the run.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_with_no_request_id_ends_the_run() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-unkeyed-{}", std::process::id()));
        let (socket, reads) = scripted_peer(&directory, 8_000, PeerScript::Unkeyed).await;
        let mut client = driver_client(socket).await;
        let mut scenario = scenario(Vec::new());
        scenario.budget.seconds = 10.;
        let driven = drive(
            &mut client,
            &scenario,
            &std::env::temp_dir(),
            &peer_model(),
            &ProducerState::default(),
            None,
        )
        .await;
        // The peer reads a request only after the driver sent it, so the
        // count is final once it has read the last one.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = std::fs::remove_dir_all(directory);
        // The budget writes `seconds`, so `error` is the run ending on the
        // call, not on its budget.
        assert_eq!(driven.stop, "error", "{:?}", driven.error);
        let error = driven.error.unwrap_or_default();
        assert!(
            error.contains("the ask_user call c1 carries no request_id"),
            "{error}"
        );
        assert!(
            reads.declines.lock().unwrap().is_empty(),
            "the run sends no decline for the call"
        );
    }

    /// A run that ends on a resend keeps the resends it sent before it: the
    /// peer takes the first call's resend and fails the second call's, and
    /// the run's count still holds the one that was sent.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_resend_keeps_the_resends_sent_before_it() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-retry-{}", std::process::id()));
        let (socket, _) = scripted_peer(&directory, 8_000, PeerScript::RetryFails).await;
        let mut client = driver_client(socket).await;
        let mut scenario = scenario(Vec::new());
        scenario.budget.seconds = 10.;
        let driven = drive(
            &mut client,
            &scenario,
            &std::env::temp_dir(),
            &peer_model(),
            &ProducerState::default(),
            None,
        )
        .await;
        let _ = std::fs::remove_dir_all(directory);
        assert_eq!(driven.stop, "error", "{:?}", driven.error);
        let error = driven.error.unwrap_or_default();
        assert!(error.contains("the decline of c2 failed"), "{error}");
        assert!(error.contains("session_reloading"), "{error}");
        assert_eq!(
            driven.decline_retries, 1,
            "the count keeps the resend the peer took before the failing one"
        );
    }

    /// A question's `ask_user` call always carries its own id
    /// (`ToolCallData.calls[].id`), so a call without one cannot be keyed to
    /// any reply: the run ends with an error naming the field rather than
    /// keeping a decline the Session cannot match and sending it again for
    /// the rest of the run.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_with_no_id_ends_the_run() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-no-id-{}", std::process::id()));
        let (socket, reads) = scripted_peer(&directory, 8_000, PeerScript::CallWithoutId).await;
        let mut client = driver_client(socket).await;
        let mut scenario = scenario(Vec::new());
        scenario.budget.seconds = 10.;
        let driven = drive(
            &mut client,
            &scenario,
            &std::env::temp_dir(),
            &peer_model(),
            &ProducerState::default(),
            None,
        )
        .await;
        // The peer reads a request only after the driver sent it, so the
        // count is final once it has read the last one.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = std::fs::remove_dir_all(directory);
        // The budget writes `seconds`, so `error` is the run ending on the
        // call, not on its budget.
        assert_eq!(driven.stop, "error", "{:?}", driven.error);
        let error = driven.error.unwrap_or_default();
        assert!(error.contains("the ask_user call carries no id"), "{error}");
        assert!(
            reads.declines.lock().unwrap().is_empty(),
            "the run sends no decline for the call"
        );
    }

    /// A line that stays busy for many quiet checks holds one idle request,
    /// not one per check: the peer reads exactly one `session.idle` while it
    /// holds the line, and the run stops idle once the line answers it
    /// (issue #1032).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_busy_line_holds_one_idle_request_across_many_checks() {
        let directory =
            std::env::temp_dir().join(format!("dim-scenario-eval-held-{}", std::process::id()));
        let (socket, reads) = scripted_peer(&directory, 8_000, PeerScript::HeldIdle).await;
        let mut client = driver_client(socket).await;
        let mut scenario = scenario(Vec::new());
        scenario.budget.seconds = 30.;
        let started = Instant::now();
        let driven = drive(
            &mut client,
            &scenario,
            &std::env::temp_dir(),
            &peer_model(),
            &ProducerState::default(),
            None,
        )
        .await;
        let elapsed = started.elapsed();
        let _ = std::fs::remove_dir_all(directory);
        assert_eq!(driven.stop, "idle", "{:?}", driven.error);
        assert!(
            elapsed >= HELD,
            "the line stayed busy for the hold: {} ms",
            elapsed.as_millis()
        );
        assert_eq!(
            reads.idles.load(Ordering::SeqCst),
            1,
            "one idle request for every check the line stayed busy"
        );
    }

    /// The `create_at` of the last `turn/end` on `main` and of the close the
    /// runner's own `session.close` writes, a `stop/interrupt` with reason
    /// `timeline closed`.
    fn last_turn_end_and_close(export: &str) -> (u64, u64) {
        let mut turn_end = None;
        let mut closed = None;
        for line in export.lines() {
            let Ok(line) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if line["kind"] != "event" || line["branch_id"] != "main" {
                continue;
            }
            let event = &line["event"];
            let create_at = event["create_at"].as_u64().unwrap_or_default();
            if event["type"] == "turn" && event["name"] == "end" {
                turn_end = Some(create_at);
            }
            if event["type"] == "stop"
                && event["name"] == "interrupt"
                && event["data"]["reason"] == "timeline closed"
            {
                closed = Some(create_at);
            }
        }
        (
            turn_end.expect("a turn ended on main"),
            closed.expect("the run closed main"),
        )
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completing_an_evaluation_preserves_another_runs_endpoint() {
        let peer = Sockets::new().unwrap();
        let address = peer.0.join("rpc.sock");
        let _listener = tokio::net::UnixListener::bind(&address).unwrap();
        let fixture = Fixture::start(
            "endpoint-isolation",
            summaries_and_judgements,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}\nrecords: {:?}",
            fixture.records(&options.out)
        );
        tokio::net::UnixStream::connect(address).await.unwrap();
    }

    /// A turn that ends while the driver waits on `session.idle` is read like
    /// any other notice: the quiet period follows it, and the run closes at
    /// least `QUIET` after the turn's end instead of at it (issue #643).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_turn_ending_during_the_idle_call_delays_the_close_by_the_quiet_period() {
        // QUIET plus half of IDLE_WAIT: the turn ends inside the driver's
        // `session.idle` call, whose deadline it beats.
        const HELD: Duration = Duration::from_millis(1500);
        /// The judge's verdict, and plain text for a run request.
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| Some(content("ok")))
        }
        /// Holds a run request's whole answer back for [`HELD`]; the judge
        /// answers at once.
        fn holds_the_turn(text: &str) -> Option<Duration> {
            (!text.contains("You check statements")).then_some(HELD)
        }
        let fixture = Fixture::holding(
            "late-turn",
            answers,
            holds_the_turn,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
            24_000,
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate is inconclusive: no comparison is a regression"),
            "{report}\nrecords: {:?}",
            fixture.records(&options.out)
        );
        for arm in ["current", "candidate"] {
            let export = std::fs::read_to_string(
                options
                    .out
                    .join("outputs")
                    .join(arm)
                    .join("case")
                    .join("run-0.timeline.jsonl"),
            )
            .unwrap();
            let (turn_end, closed) = last_turn_end_and_close(&export);
            assert!(
                closed >= turn_end + QUIET.as_millis() as u64,
                "{arm}: the run closed {} ms after its last turn ended, short of QUIET",
                closed.saturating_sub(turn_end)
            );
        }
    }

    /// Both arms serve the product's default plugins: a Goal method set in
    /// the scenario's `calls` answers on its Session, so the run reaches its
    /// turn instead of failing the call (issue #612, acceptance 1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
    async fn node_default_plugins_load_and_a_goal_method_answers() {
        let fixture = Fixture::start(
            "goal-plugin",
            summaries_and_judgements,
            goal_call_scenario(),
            &[],
        )
        .await;
        let defaults = repository().join("runtime/default-plugins.json");
        let options = fixture.plugin_options(fixture.directory.join("out"), defaults);
        for arm in ["current", "candidate"] {
            let record = run_arm(&fixture, &options, arm, &Overrides::default(), 0).await;
            assert_eq!(record.error, None, "{arm}");
            assert!(!record.failed, "{arm}");
            assert!(record.turns >= 1, "{arm}: the message's turn ran");
        }
    }

    /// A scenario's `calls` run before its message: the goal it sets is in
    /// the first request of both arms (issue #612, acceptance 2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
    async fn node_a_calls_step_sets_a_goal_before_the_message() {
        let fixture = Fixture::start(
            "goal-calls",
            summaries_and_judgements,
            goal_call_scenario(),
            &[],
        )
        .await;
        let defaults = repository().join("runtime/default-plugins.json");
        let options = fixture.plugin_options(fixture.directory.join("out"), defaults);
        for arm in ["current", "candidate"] {
            let record = run_arm(&fixture, &options, arm, &Overrides::default(), 0).await;
            assert_eq!(record.error, None, "{arm}");
            let requests = fixture.arm(arm);
            assert!(!requests.is_empty(), "{arm}: the turn made a request");
            assert!(
                requests[0].contains("dim-goal") && requests[0].contains("Keep the suite green."),
                "{arm}: the first request carries the goal the call set"
            );
        }
    }

    /// A turn that ends after the quiet period has passed hands its notices to
    /// the driver: Goal's continuation, written at that turn's end, drives a
    /// turn of its own in the same run, and the budget counts it (issue #643).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
    async fn node_a_continuation_after_a_held_turn_runs_in_the_same_run() {
        // QUIET plus half of IDLE_WAIT: a held turn ends inside the driver's
        // `session.idle` call, whose deadline it beats.
        const HELD: Duration = Duration::from_millis(1500);
        /// The judge's verdict, and plain text for a run request.
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| Some(content("ok")))
        }
        /// Holds a run request's whole answer back for [`HELD`], except one
        /// that carries Goal's continuation, which is answered at once; the
        /// judge answers at once.
        fn holds_the_turn(text: &str) -> Option<Duration> {
            (!text.contains("You check statements")
                && !text.contains("Continue working toward the active goal"))
            .then_some(HELD)
        }
        let fixture = Fixture::holding(
            "goal-continuation",
            answers,
            holds_the_turn,
            json!({
                "message": "Continue the work.",
                "calls": [{"method": "session.goal.set", "params": {"objective": "Keep the suite green."}}],
                // The goal the calls step sets drives a turn of its own before
                // the message's, so the continuation is the run's third turn
                // and the budget stops there, not at the turn that wrote it.
                "budget": {"turns": 3, "seconds": 60, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
            24_000,
        )
        .await;
        let defaults = repository().join("runtime/default-plugins.json");
        let options = fixture.plugin_options(fixture.directory.join("out"), defaults);
        for arm in ["current", "candidate"] {
            let record = run_arm(&fixture, &options, arm, &Overrides::default(), 0).await;
            assert_eq!(record.error, None, "{arm}");
            assert_eq!(record.stop, "turns", "{arm}");
            assert_eq!(record.turns, 3, "{arm}: Goal's continuation ran");
            let export = std::fs::read_to_string(
                options
                    .out
                    .join("outputs")
                    .join(arm)
                    .join("case")
                    .join("run-0.timeline.jsonl"),
            )
            .unwrap();
            let continuation = export.lines().any(|line| {
                let Ok(line) = serde_json::from_str::<Value>(line) else {
                    return false;
                };
                line["kind"] == "event"
                    && line["branch_id"] == "main"
                    && line["event"]["type"] == "input"
                    && line["event"]["name"] == "message"
                    && line["event"]["data"]["blocks"]
                        .as_array()
                        .is_some_and(|blocks| {
                            blocks.iter().any(|block| {
                                block["text"]
                                    .as_str()
                                    .is_some_and(|text| text.contains(r#"kind="continuation""#))
                            })
                        })
            });
            assert!(
                continuation,
                "{arm}: Goal's continuation input is in the export"
            );
        }
    }

    /// A scenario's source and event reach a run: the subscription writes the
    /// source's guide as a live block, the event is published at the tool
    /// call and delivered to `main` with its marker, the reply tool the
    /// source names records the answer, and the request after the event
    /// carries the order-130 entry — the candidate arm's request carrying the
    /// candidate text in place of the shipped `ENTRY` (EventHub design A.6).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_event_reaches_the_run_and_the_reply_tool_records_the_answer() {
        const CANDIDATE: &str = "CANDIDATE EVENTHUB ENTRY TEXT";
        const GUIDE: &str = "The team cannot see this conversation; answer through chat_reply.";
        const REPLY: &str = "PARSE-SKIPS-EMPTY-LINES.";
        const QUESTION: &str = "What does parse() do with an empty line?";
        /// The judge's verdict, a `chat_reply` call the first time a request
        /// carries the event, and a `read` of the workspace file until its
        /// text is in the request.
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| {
                if text.contains(QUESTION) && !text.contains(REPLY) {
                    return Some(tool_call(
                        "chat_reply",
                        &json!({"messageId": "m-1", "text": REPLY}).to_string(),
                    ));
                }
                (!text.contains(FILE_TEXT)).then(|| tool_call("read", r#"{"path":"notes.txt"}"#))
            })
        }
        let fixture = Fixture::start(
            "eventhub-entry",
            answers,
            json!({
                "message": "Read notes.txt and tell me what it says.",
                "sources": [{
                    "name": "chat",
                    "guide": GUIDE,
                    "types": [{
                        "type": "chat.message",
                        "description": "A message from the team's chat.",
                        "mode": "queue"
                    }],
                    "subscribe": {"types": ["chat.message"]},
                    "replyTool": "chat_reply"
                }],
                "events": [{
                    "source": "chat",
                    "type": "chat.message",
                    "id": "m-1",
                    "title": "The failing test",
                    "body": QUESTION,
                    "whenToolCall": "read"
                }],
                "budget": {"turns": 6, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let candidate = overrides(&pairs(&[("eventhub/entry", CANDIDATE)])).unwrap();
        for (arm, overrides) in [("current", Overrides::default()), ("candidate", candidate)] {
            let record = run_arm(&fixture, &options, arm, &overrides, 0).await;
            assert_eq!(record.error, None, "{arm}");
            assert!(!record.failed, "{arm}");
            assert_eq!(record.events.len(), 1, "{arm}: one event was published");
            let receipt = &record.events[0].receipt;
            assert_eq!(
                receipt["deliveries"][0]["status"],
                json!("delivered"),
                "{arm}: {receipt}"
            );
            assert_eq!(
                record.reply_calls.len(),
                1,
                "{arm}: the reply tool was called once: {:?}",
                record.reply_calls
            );
            assert_eq!(record.reply_calls[0].tool, "chat_reply", "{arm}");
            assert_eq!(record.reply_calls[0].message_id, "m-1", "{arm}");
            assert_eq!(record.reply_calls[0].text, REPLY, "{arm}");
            let requests = fixture.arm(arm);
            assert!(
                requests.iter().any(|request| carried(
                    request,
                    "<dim-event source=\"chat\" type=\"chat.message\" id=\"m-1\">"
                )),
                "{arm}: the event's input carries the dim-event marker"
            );
            assert!(
                requests.iter().any(|request| carried(
                    request,
                    &format!("<dim-event-guide source=\"chat\">{GUIDE}</dim-event-guide>")
                )),
                "{arm}: the guide block is live"
            );
            let entry = dim_event_hub::ENTRY;
            if arm == "current" {
                assert!(
                    requests.iter().any(|request| carried(request, entry)),
                    "the current arm's request carries the shipped entry"
                );
                assert!(
                    requests.iter().all(|request| !carried(request, CANDIDATE)),
                    "the current arm never carries the candidate text"
                );
            } else {
                assert!(
                    requests.iter().any(|request| carried(request, CANDIDATE)),
                    "the candidate arm's request carries the candidate text"
                );
                assert!(
                    requests.iter().all(|request| !carried(request, entry)),
                    "the candidate arm never carries the shipped entry"
                );
            }
        }
    }

    /// A scenario's source may start unsubscribed: the model subscribes
    /// through the `events` tool, and the event fires `afterSubscribedSeconds`
    /// after that subscription first exists, arriving on the branch with its
    /// marker (EventHub design A.4).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_model_subscribes_through_the_tool_and_the_event_fires_after_that() {
        const GUIDE: &str = "CI publishes a failure notice when a build breaks.";
        const BODY: &str = "The build failed on the release branch.";
        const DESCRIPTION: &str = "Subscribe only when the user asks you to follow events.";
        /// The judge's verdict, the `events` subscribe call until its result
        /// is in the request, a read of the workspace file until the event
        /// arrives, and plain text after.
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| {
                if !text.contains("sub_") {
                    return Some(tool_call(
                        "events",
                        &json!({"action": "subscribe", "types": ["ci.*"], "mode": "steer"})
                            .to_string(),
                    ));
                }
                (!text.contains(BODY)).then(|| tool_call("read", r#"{"path":"build.txt"}"#))
            })
        }
        let fixture = Fixture::start(
            "eventhub-tool",
            answers,
            json!({
                "message": "Subscribe to CI failures, then read build.txt.",
                "sources": [{
                    "name": "ci",
                    "guide": GUIDE,
                    "types": [{
                        "type": "ci.failure",
                        "description": "A failed build.",
                        "mode": "queue"
                    }]
                }],
                "events": [{
                    "source": "ci",
                    "type": "ci.failure",
                    "id": "f-1",
                    "title": "The build failed",
                    "body": BODY,
                    "afterSubscribedSeconds": 1
                }],
                "budget": {"turns": 6, "seconds": 60, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[("build.txt", "build output\n")],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let record = run_arm(&fixture, &options, "current", &Overrides::default(), 0).await;
        assert_eq!(record.error, None);
        assert!(!record.failed);
        assert_eq!(record.events.len(), 1, "one event was published");
        let receipt = &record.events[0].receipt;
        assert_eq!(
            receipt["deliveries"][0]["status"],
            json!("delivered"),
            "{receipt}"
        );
        assert!(
            receipt["deliveries"][0]["subscriptionIds"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty()),
            "the model's own subscription carried the event: {receipt}"
        );
        let requests = fixture.arm("current");
        assert!(
            requests.iter().any(|request| carried(request, DESCRIPTION)),
            "the request offers the events tool with its description"
        );
        assert!(
            requests.iter().any(|request| carried(
                request,
                "<dim-event source=\"ci\" type=\"ci.failure\" id=\"f-1\">"
            )),
            "the event's input carries the dim-event marker"
        );
    }

    /// A subscription whose `match` excludes the event directs no delivery:
    /// a started clock whose publish matches nobody is the run's outcome,
    /// recorded, not a failed run (EventHub design A.3, A.4).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_event_no_subscription_takes_is_recorded_not_failed() {
        const GUIDE: &str = "CI publishes a failure notice when a build breaks.";
        /// The judge's verdict, one `events` subscribe whose `match`
        /// excludes the event, and plain text after.
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| {
                (!text.contains("sub_")).then(|| {
                    tool_call(
                        "events",
                        &json!({
                            "action": "subscribe",
                            "types": ["ci.*"],
                            "match": {"branch": "release"},
                            "mode": "steer"
                        })
                        .to_string(),
                    )
                })
            })
        }
        let fixture = Fixture::start(
            "eventhub-tool-missed",
            answers,
            json!({
                "message": "Subscribe to CI failures, then leave it for now.",
                "sources": [{
                    "name": "ci",
                    "guide": GUIDE,
                    "types": [{
                        "type": "ci.failure",
                        "description": "A failed build.",
                        "mode": "queue"
                    }]
                }],
                "events": [{
                    "source": "ci",
                    "type": "ci.failure",
                    "id": "f-1",
                    "title": "The build failed",
                    "body": "The build failed on the release branch.",
                    "attributes": {"branch": "main"},
                    "afterSubscribedSeconds": 1
                }],
                "budget": {"turns": 6, "seconds": 30, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let record = run_arm(&fixture, &options, "current", &Overrides::default(), 0).await;
        assert_eq!(record.error, None);
        assert!(!record.failed, "the miss is the run's outcome");
        assert_eq!(record.stop, "idle", "no turn is expected for the miss");
        assert_eq!(record.events.len(), 1, "one event was published");
        assert_eq!(
            record.events[0].receipt["deliveries"],
            json!([]),
            "the model left no subscription: {}",
            record.events[0].receipt
        );
    }

    /// A model's `append` subscription delivers without starting a turn, so
    /// the run ends idle rather than on its seconds budget (EventHub design
    /// §3.1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_append_delivery_from_a_model_subscription_starts_no_turn() {
        const GUIDE: &str = "CI publishes a failure notice when a build breaks.";
        /// The judge's verdict, one `events` subscribe call with `append`,
        /// and plain text after.
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| {
                (!text.contains("sub_")).then(|| {
                    tool_call(
                        "events",
                        &json!({"action": "subscribe", "types": ["ci.*"], "mode": "append"})
                            .to_string(),
                    )
                })
            })
        }
        let fixture = Fixture::start(
            "eventhub-tool-append",
            answers,
            json!({
                "message": "Subscribe to CI failures in the background.",
                "sources": [{
                    "name": "ci",
                    "guide": GUIDE,
                    "types": [{
                        "type": "ci.failure",
                        "description": "A failed build.",
                        "mode": "queue"
                    }]
                }],
                "events": [{
                    "source": "ci",
                    "type": "ci.failure",
                    "id": "f-1",
                    "title": "The build failed",
                    "body": "The build failed on the release branch.",
                    "afterSubscribedSeconds": 1
                }],
                "budget": {"turns": 6, "seconds": 30, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let record = run_arm(&fixture, &options, "current", &Overrides::default(), 0).await;
        assert_eq!(record.error, None);
        assert!(!record.failed);
        assert_eq!(
            record.stop, "idle",
            "append starts no turn, so no budget stop"
        );
        // The run is isolated: this fixture path serves the run in the
        // child with the sandbox and the file-tool boundary, and every
        // preflight check passed for the run to finish idle.
        let checks = recorded_checks(&options.out, "current");
        for check in &checks {
            assert!(check.passed, "{check:?}");
        }
        assert_eq!(record.events.len(), 1, "one event was published");
        assert_eq!(
            record.events[0].receipt["deliveries"][0]["status"],
            json!("delivered"),
            "{}",
            record.events[0].receipt
        );
        assert!(
            record.events[0].receipt["deliveries"][0]["subscriptionIds"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty()),
            "the model's own subscription carried the event: {}",
            record.events[0].receipt
        );
    }

    /// The scenario's own `append` subscription delivers without a turn, so
    /// a timed event leaves the run idle rather than running to its budget
    /// (EventHub design §3.1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_append_delivery_from_the_scenario_starts_no_turn() {
        let fixture = Fixture::start(
            "eventhub-tool-append-declared",
            judged,
            json!({
                "message": "Say hello.",
                "sources": [{
                    "name": "ci",
                    "guide": "CI publishes a failure notice when a build breaks.",
                    "types": [{
                        "type": "ci.failure",
                        "description": "A failed build.",
                        "mode": "queue"
                    }],
                    "subscribe": {"types": ["ci.failure"], "mode": "append"}
                }],
                "events": [{
                    "source": "ci",
                    "type": "ci.failure",
                    "id": "f-1",
                    "title": "The build failed",
                    "body": "The build failed on the release branch.",
                    "afterSeconds": 1
                }],
                "budget": {"turns": 4, "seconds": 30, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let record = run_arm(&fixture, &options, "current", &Overrides::default(), 0).await;
        assert_eq!(record.error, None);
        assert!(!record.failed);
        assert_eq!(
            record.stop, "idle",
            "append starts no turn, so no budget stop"
        );
        assert_eq!(record.events.len(), 1, "one event was published");
        assert_eq!(
            record.events[0].receipt["deliveries"][0]["status"],
            json!("delivered"),
            "{}",
            record.events[0].receipt
        );
    }

    /// A subscription's `match` can exclude an event, and it then directs no
    /// delivery: the mode comes from the subscriptions the receipt lists, so
    /// a `steer` subscription that excluded the event starts no turn. The
    /// event fires on a later tool call the model makes only after both
    /// subscribe results are in its request (EventHub design §3.1, A.2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_delivery_takes_its_mode_from_the_subscriptions_that_directed_it() {
        const GUIDE: &str = "CI publishes a failure notice when a build breaks.";
        /// The judge's verdict, a `steer` subscribe whose `match` excludes
        /// the event, an `append` subscribe, a read once both ids are in the
        /// request, and plain text after.
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| match text.matches("sub_").count() {
                0 => Some(tool_call(
                    "events",
                    &json!({
                        "action": "subscribe",
                        "types": ["ci.*"],
                        "match": {"branch": "release"},
                        "mode": "steer"
                    })
                    .to_string(),
                )),
                1 => Some(tool_call(
                    "events",
                    &json!({"action": "subscribe", "types": ["ci.failure"], "mode": "append"})
                        .to_string(),
                )),
                _ => (!text.contains("build output"))
                    .then(|| tool_call("read", r#"{"path":"build.txt"}"#)),
            })
        }
        let fixture = Fixture::start(
            "eventhub-tool-directed",
            answers,
            json!({
                "message": "Subscribe to CI failures in the background.",
                "sources": [{
                    "name": "ci",
                    "guide": GUIDE,
                    "types": [{
                        "type": "ci.failure",
                        "description": "A failed build.",
                        "mode": "queue"
                    }]
                }],
                "events": [{
                    "source": "ci",
                    "type": "ci.failure",
                    "id": "f-1",
                    "title": "The build failed",
                    "body": "The build failed on the release branch.",
                    "attributes": {"branch": "main"},
                    "whenToolCall": "read"
                }],
                "budget": {"turns": 6, "seconds": 30, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[("build.txt", "build output\n")],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let record = run_arm(&fixture, &options, "current", &Overrides::default(), 0).await;
        assert_eq!(record.error, None);
        assert!(!record.failed);
        assert_eq!(
            record.stop, "idle",
            "the append delivery starts no turn, so no budget stop"
        );
        // The run is isolated: this fixture path serves the run in the
        // child with the sandbox and the file-tool boundary, and every
        // preflight check passed for the run to finish idle.
        let checks = recorded_checks(&options.out, "current");
        for check in &checks {
            assert!(check.passed, "{check:?}");
        }
        assert_eq!(record.events.len(), 1, "one event was published");
        let receipt = &record.events[0].receipt;
        assert_eq!(
            receipt["deliveries"][0]["status"],
            json!("delivered"),
            "{receipt}"
        );
        let requests = fixture.arm("current");
        let ids = subscription_ids(requests.last().expect("the run made requests"));
        assert_eq!(ids.len(), 2, "the model made two subscriptions: {ids:?}");
        assert_eq!(
            receipt["deliveries"][0]["subscriptionIds"],
            json!([ids[1]]),
            "only the append subscription directed the delivery: {receipt}"
        );
    }

    /// A model that never subscribes cannot make an `afterSubscribedSeconds`
    /// event fire: the event is not pending, so the run ends idle with no
    /// event published rather than on its seconds budget (EventHub design
    /// A.4).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_run_whose_model_never_subscribed_ends_idle() {
        /// The judge's verdict; the run's own answer is plain text, and the
        /// model never calls the `events` tool.
        fn answers(text: &str) -> Option<Value> {
            judged(text)
        }
        let fixture = Fixture::start(
            "eventhub-tool-never-subscribed",
            answers,
            json!({
                "message": "Say hello.",
                "sources": [{
                    "name": "ci",
                    "guide": "CI publishes a failure notice when a build breaks.",
                    "types": [{
                        "type": "ci.failure",
                        "description": "A failed build.",
                        "mode": "queue"
                    }]
                }],
                "events": [{
                    "source": "ci",
                    "type": "ci.failure",
                    "id": "f-1",
                    "title": "The build failed",
                    "body": "The build failed on the release branch.",
                    "afterSubscribedSeconds": 1
                }],
                "budget": {"turns": 4, "seconds": 30, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let record = run_arm(&fixture, &options, "current", &Overrides::default(), 0).await;
        assert_eq!(record.error, None);
        assert!(!record.failed);
        assert_eq!(
            record.stop, "idle",
            "an unstarted event cannot hold the run open"
        );
        assert!(
            record.events.is_empty(),
            "the event was never published: {:?}",
            record.events
        );
    }

    /// The same for a tool-triggered delivery: the trigger's tool call
    /// publishes the event of a model `append` subscription, which starts no
    /// turn (EventHub design §3.1, A.2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_triggered_append_delivery_starts_no_turn() {
        /// The judge's verdict, the `events` subscribe call until its result
        /// is in the request, the trigger's `read` until its result is, and
        /// plain text after.
        fn answers(text: &str) -> Option<Value> {
            judged(text).or_else(|| {
                if !text.contains("sub_") {
                    return Some(tool_call(
                        "events",
                        &json!({"action": "subscribe", "types": ["ci.failure"], "mode": "append"})
                            .to_string(),
                    ));
                }
                (!text.contains("build output"))
                    .then(|| tool_call("read", r#"{"path":"build.txt"}"#))
            })
        }
        let fixture = Fixture::start(
            "eventhub-tool-triggered-append",
            answers,
            json!({
                "message": "Subscribe to CI failures with append, then read build.txt.",
                "sources": [{
                    "name": "ci",
                    "guide": "CI publishes a failure notice when a build breaks.",
                    "types": [{
                        "type": "ci.failure",
                        "description": "A failed build.",
                        "mode": "queue"
                    }]
                }],
                "events": [{
                    "source": "ci",
                    "type": "ci.failure",
                    "id": "f-1",
                    "title": "The build failed",
                    "body": "The build failed on the release branch.",
                    "whenToolCall": "read"
                }],
                "budget": {"turns": 4, "seconds": 30, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[("build.txt", "build output\n")],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let record = run_arm(&fixture, &options, "current", &Overrides::default(), 0).await;
        assert_eq!(record.error, None);
        assert!(
            !record.failed,
            "the delivery is correct and nothing is outstanding"
        );
        assert_eq!(
            record.stop, "idle",
            "an append delivery starts no turn: {:?}",
            record.events
        );
        // The run is isolated: this fixture path serves the run in the
        // child with the sandbox and the file-tool boundary, and every
        // preflight check passed for the run to finish idle.
        let checks = recorded_checks(&options.out, "current");
        for check in &checks {
            assert!(check.passed, "{check:?}");
        }
        assert_eq!(record.events.len(), 1, "one event was published");
        let receipt = &record.events[0].receipt;
        assert_eq!(
            receipt["deliveries"][0]["status"],
            json!("delivered"),
            "{receipt}"
        );
        assert!(
            receipt["deliveries"][0]["subscriptionIds"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty()),
            "the model's own subscription carried the event: {receipt}"
        );
    }

    /// A candidate for a TypeScript plugin's text replaces that member of
    /// the plugin's factory configuration: the candidate arm's request
    /// carries it, and the current arm carries the shipped text (issue #612,
    /// acceptance 3). The plugin's tool reaches the model too: a run's
    /// Sessions mount the set the product probe declares, so both arms'
    /// requests declare `fixture_prompt`, which the harness itself never
    /// names.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
    async fn node_a_plugin_candidate_reaches_the_request() {
        const SHIPPED: &str = "text-injector fixture: the shipped description";
        const CANDIDATE: &str = "CANDIDATE PLUGIN TEXT";
        const FIXTURE_TEXTS: &[(&str, TextMember)] = &[(
            "fixture/prompt",
            TextMember::Plugin {
                plugin: "text-injector",
                member: "texts.prompt",
            },
        )];
        let fixture = Fixture::start(
            "plugin-candidate",
            summaries_and_judgements,
            json!({
                "message": "Say hello.",
                "budget": {"turns": 2, "seconds": 60, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let defaults = fixture.directory.join("plugin-defaults.json");
        std::fs::write(
            &defaults,
            json!({"plugins": {"text-injector": repository().join("runtime/fixtures/text-injector")}})
                .to_string(),
        )
        .unwrap();
        let options = fixture.plugin_options(fixture.directory.join("out"), defaults);
        let candidate =
            overrides_in(FIXTURE_TEXTS, &pairs(&[("fixture/prompt", CANDIDATE)])).unwrap();
        assert_eq!(
            candidate.plugin_configs["text-injector"]["texts"]["prompt"],
            json!(CANDIDATE),
            "the table puts the candidate at the plugin's factory member"
        );
        for (arm, overrides) in [("current", Overrides::default()), ("candidate", candidate)] {
            let record = run_arm(&fixture, &options, arm, &overrides, 0).await;
            assert_eq!(record.error, None, "{arm}");
            assert!(!record.failed, "{arm}");
        }
        let current = fixture.arm("current");
        let candidate = fixture.arm("candidate");
        assert!(
            current.iter().any(|request| request.contains(SHIPPED)),
            "the current arm carries the shipped text"
        );
        assert!(
            current.iter().all(|request| !request.contains(CANDIDATE)),
            "the current arm never carries the candidate text"
        );
        assert!(
            candidate.iter().any(|request| request.contains(CANDIDATE)),
            "the candidate arm's request carries the candidate text"
        );
        assert!(
            candidate.iter().all(|request| !request.contains(SHIPPED)),
            "the candidate arm never carries the shipped text"
        );
        for (arm, requests) in [("current", &current), ("candidate", &candidate)] {
            let offered = offered_tools(&requests[0]);
            assert!(
                offered.iter().any(|tool| tool == "fixture_prompt"),
                "{arm}: the run's request declares the plugin's tool: {offered:?}"
            );
        }
    }

    /// The serve child of a run when the runner is this test binary: the
    /// parent starts this binary with `--exact` and this name, and the
    /// params file in `DIM_EVAL_SERVE_PARAMS`. A normal run of the suite
    /// executes the entry with no params and passes as a no-op.
    #[test]
    fn eval_server_child() {
        let Some(params) = std::env::var_os("DIM_EVAL_SERVE_PARAMS") else {
            return;
        };
        let params = PathBuf::from(params);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        // The same child entry as the product binary's `scenario-eval-serve`:
        // the mock, the model configuration with its own preflight provider,
        // and the run.
        runtime
            .block_on(serve_entry(Some(params.to_string_lossy().into_owned())))
            .unwrap();
    }

    /// The preflight checks a run recorded, from its report under the output
    /// root.
    fn recorded_checks(out: &Path, arm: &str) -> Vec<PreflightCheck> {
        let path = out
            .join("outputs")
            .join(arm)
            .join("case")
            .join("run-0.preflight.json");
        let text = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn check<'a>(checks: &'a [PreflightCheck], name: &str) -> &'a PreflightCheck {
        checks
            .iter()
            .find(|check| check.name == name)
            .unwrap_or_else(|| panic!("no {name} check in {checks:?}"))
    }

    /// The scenario both arms of the isolation tests run: a single turn that
    /// leaves a transcript the judge can find.
    fn isolated_scenario() -> Value {
        json!({
            "description": "Isolation",
            "message": "Run the probes.",
            "budget": {"turns": 4, "seconds": 120, "window": 24000},
            "probes": [{"id": "ran", "kind": "must_show",
                        "text": "the run has a transcript"}]
        })
    }

    /// An isolated run passes its preflight, and the same run without
    /// isolation shows the check observing the operator's environment
    /// (ablation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_isolated_run_prints_the_disposable_home() {
        let fixture = Fixture::start(
            "isolated-home",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        let records = fixture.records(&options.out);
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert!(check(&recorded_checks(&options.out, "current"), "home").passed);
        }
        // Ablation: without the isolation switches the check observes the
        // operator's home, not the run's disposable one, and fails.
        let ablated = fixture.directory.join("ablated");
        let mut ablated_options = fixture.options(ablated.clone());
        ablated_options.isolate = false;
        let _ = fixture.evaluate(&ablated_options).await.unwrap();
        assert!(!check(&recorded_checks(&ablated, "current"), "home").passed);
    }

    /// The model configuration names a credential variable: an isolated
    /// run's exec environment hides it, and an unisolated one shows it
    /// (ablation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_run_exec_environment_shows_no_credential_name() {
        // The credential under test: only its name must be checked, never
        // its value.
        unsafe { std::env::set_var("DIM_FIXTURE_CREDENTIAL", "fixture-secret") };
        let fixture = Fixture::start(
            "isolated-credential",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let models = std::fs::read_to_string(&fixture.models).unwrap();
        let mut models: Value = serde_json::from_str(&models).unwrap();
        models["providers"].as_array_mut().unwrap()[0]["apiKeyEnv"] =
            json!("DIM_FIXTURE_CREDENTIAL");
        std::fs::write(
            &fixture.models,
            serde_json::to_string_pretty(&models).unwrap(),
        )
        .unwrap();
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        let records = fixture.records(&options.out);
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert!(check(&recorded_checks(&options.out, "current"), "credentials").passed);
        }
        // Ablation: the exec environment carries the credential name.
        let ablated = fixture.directory.join("ablated");
        let mut ablated_options = fixture.options(ablated.clone());
        ablated_options.isolate = false;
        let _ = fixture.evaluate(&ablated_options).await.unwrap();
        assert!(!check(&recorded_checks(&ablated, "current"), "credentials").passed);
    }

    /// An isolated run refuses a command and a file-tool read that reach
    /// outside the reads the boundary allows — the marker beside the
    /// workspace and the one under the outputs root, where the judge views
    /// and the sibling runs' records live — and an unisolated one reads
    /// both markers (ablation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_isolated_run_refuses_a_command_that_reads_outside() {
        let fixture = Fixture::start(
            "isolated-cat",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        let records = fixture.records(&options.out);
        for record in &records {
            // The checks first: an ablated run fails here, at the line of
            // the check that caught the mechanism, not at the stop line.
            let checks = recorded_checks(&options.out, "current");
            assert!(check(&checks, "exec-outside").passed, "{checks:?}");
            assert!(check(&checks, "read-outside").passed, "{checks:?}");
            assert!(check(&checks, "exec-outputs").passed, "{checks:?}");
            assert!(check(&checks, "read-outputs").passed, "{checks:?}");
            assert_eq!(record["stop"], "idle", "{record}");
        }
        let ablated = fixture.directory.join("ablated");
        let mut ablated_options = fixture.options(ablated.clone());
        ablated_options.isolate = false;
        let _ = fixture.evaluate(&ablated_options).await.unwrap();
        let checks = recorded_checks(&ablated, "current");
        assert!(!check(&checks, "exec-outside").passed);
        assert!(!check(&checks, "read-outside").passed);
        assert!(!check(&checks, "exec-outputs").passed);
        assert!(!check(&checks, "read-outputs").passed);
    }

    /// An isolated run refuses an absolute path in the operator's home for
    /// `exec` and for `read`, and an unisolated one reaches it (ablation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_isolated_run_refuses_the_operator_home() {
        let fixture = Fixture::start(
            "isolated-home-read",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        let records = fixture.records(&options.out);
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert!(
                check(
                    &recorded_checks(&options.out, "current"),
                    "exec-operator-home"
                )
                .passed
            );
            assert!(
                check(
                    &recorded_checks(&options.out, "current"),
                    "read-operator-home"
                )
                .passed
            );
        }
        let ablated = fixture.directory.join("ablated");
        let mut ablated_options = fixture.options(ablated.clone());
        ablated_options.isolate = false;
        let _ = fixture.evaluate(&ablated_options).await.unwrap();
        assert!(!check(&recorded_checks(&ablated, "current"), "exec-operator-home").passed);
        assert!(!check(&recorded_checks(&ablated, "current"), "read-operator-home").passed);
    }

    /// The run's socket lies outside the run root and the workspace: the
    /// model's commands reach only what the sandbox and the file-root
    /// boundary grant, so a socket inside the workspace would be
    /// reachable. (A socket under the run root but outside the workspace
    /// stays unreachable too — the run root is granted to nothing — so
    /// the layout is what the socket check rests on, and this test pins
    /// it.)
    #[test]
    fn the_run_socket_lies_outside_the_run_root_and_workspace() {
        let out = PathBuf::from("/tmp/out");
        let options = Options {
            out: out.clone(),
            scenarios: PathBuf::new(),
            texts: Vec::new(),
            model: model(),
            judge: model(),
            runs: 1,
            jobs: 1,
            model_config: None,
            isolate: true,
            fail_preflight: false,
            offer_plugin: false,
            extra_tool: false,
            fail_child: None,
            fail_child_holds_stdout: false,
            defaults: None,
            node: None,
            js_runtime: None,
            rejudge: None,
            select: Vec::new(),
            pool: Vec::new(),
        };
        let arm = "current";
        let scenario = scenario(Vec::new());
        let run = 0usize;
        let run_root = run_root(&options, arm, &scenario, run);
        let socket = run_socket(&options, arm, &scenario, run);
        assert!(
            !socket.starts_with(&run_root),
            "the socket must not live under the run root: {socket:?}"
        );
        assert!(
            !socket.starts_with(workspace_of(&run_root)),
            "the socket must not live under the workspace: {socket:?}"
        );
    }

    /// An isolated run's commands cannot reach the run's socket, and an
    /// unisolated one can (ablation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_isolated_run_commands_cannot_reach_the_socket() {
        let fixture = Fixture::start(
            "isolated-socket",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        let records = fixture.records(&options.out);
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert!(check(&recorded_checks(&options.out, "current"), "socket").passed);
        }
        let ablated = fixture.directory.join("ablated");
        let mut ablated_options = fixture.options(ablated.clone());
        ablated_options.isolate = false;
        let _ = fixture.evaluate(&ablated_options).await.unwrap();
        assert!(!check(&recorded_checks(&ablated, "current"), "socket").passed);
    }

    /// A failed preflight aborts the run as a harness failure, not a model
    /// result, and without the isolation switches the run proceeds
    /// (ablation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_preflight_aborts_the_run() {
        let fixture = Fixture::start(
            "isolated-fault",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let mut faulted = fixture.options(fixture.directory.join("faulted"));
        faulted.fail_preflight = true;
        let report = fixture.evaluate(&faulted).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        let records = fixture.records(&faulted.out);
        for record in &records {
            assert_eq!(record["stop"], "error", "{record}");
            assert!(
                record["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("harness failure: preflight"),
                "{record}"
            );
        }
        // Ablation: without the isolation switches the fault is not
        // enforced and the run proceeds.
        let ablated = fixture.directory.join("ablated");
        let mut ablated_options = fixture.options(ablated.clone());
        ablated_options.isolate = false;
        ablated_options.fail_preflight = true;
        let _ = fixture.evaluate(&ablated_options).await.unwrap();
        let records = fixture.records(&ablated);
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
        }
    }

    /// The served process registers no `plugin` tool (`ServerConfig::
    /// plugin_tools` is false there), so the preflight Session's model
    /// request carries the run's tool set without it: with the run's mount
    /// list, and — left unset, the ablation that once offered the tool —
    /// with every registered tool. Both pass the tools check.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_isolated_run_offers_no_plugin_tool() {
        let fixture = Fixture::start(
            "isolated-plugin",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        for record in fixture.records(&options.out) {
            assert_eq!(record["stop"], "idle", "{record}");
            let checks = recorded_checks(&options.out, "current");
            let tools = check(&checks, "tools");
            assert!(tools.passed, "{tools:?}");
            assert!(
                tools.detail.contains("without the plugin tool"),
                "{tools:?}"
            );
        }
        // The mount list is not what removes the tool: left unset, the
        // Session declares every registered tool and still no `plugin`.
        let unset = fixture.directory.join("unset");
        let mut unset_options = fixture.options(unset.clone());
        unset_options.isolate = false;
        unset_options.offer_plugin = true;
        let _ = fixture.evaluate(&unset_options).await.unwrap();
        let checks = recorded_checks(&unset, "current");
        let tools = check(&checks, "tools");
        assert!(tools.passed, "{tools:?}");
        assert!(
            tools.detail.contains("without the plugin tool"),
            "{tools:?}"
        );
    }

    /// The product's own tool set is checked at run time: a second
    /// preflight Session, created without a mount list, declares what the
    /// served assembly offers, and the `tools-product` check keeps that
    /// set minus `plugin` equal to the run list. The ablation (an entry
    /// removed from `RUN_TOOLS`, run once and reverted) fails the check
    /// and aborts the run as a harness failure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_isolated_run_checks_the_product_tool_set_against_the_run_list() {
        let fixture = Fixture::start(
            "isolated-tools-product",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        for record in fixture.records(&options.out) {
            let checks = recorded_checks(&options.out, "current");
            let product = check(&checks, "tools-product");
            assert!(product.passed, "{product:?}");
            assert_eq!(record["stop"], "idle", "{record}");
        }
    }

    /// A tool `RUN_TOOLS` does not name, contributed by a part no plugin
    /// defaults contribute, fails `tools-product`: the run list stays a
    /// reviewed whitelist at run time, so a product tool can never reach a
    /// live run unreviewed. The test hook contributes `fixture_extra`; the
    /// unisolated run records the failed check and proceeds, the isolated
    /// one aborts as a harness failure before the model's first turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_isolated_run_rejects_a_tool_outside_the_run_list() {
        let fixture = Fixture::start(
            "isolated-tools-extra",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let ablated = fixture.directory.join("ablated");
        let mut options = fixture.options(ablated.clone());
        options.isolate = false;
        options.extra_tool = true;
        let _ = fixture.evaluate(&options).await.unwrap();
        let checks = recorded_checks(&ablated, "current");
        let product = check(&checks, "tools-product");
        assert!(!product.passed, "{product:?}");
        assert!(product.detail.contains("fixture_extra"), "{product:?}");
        let aborted = fixture.directory.join("aborted");
        let mut options = fixture.options(aborted.clone());
        options.extra_tool = true;
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        for record in fixture.records(&aborted) {
            assert_eq!(record["stop"], "error", "{record}");
            assert!(
                record["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("harness failure: preflight"),
                "{record}"
            );
        }
        let checks = recorded_checks(&aborted, "current");
        let product = check(&checks, "tools-product");
        assert!(!product.passed, "{product:?}");
    }

    /// The differential probe's child: served as a run child is served but
    /// without the plugin defaults, its probe Session declares the assembly's
    /// native tool set — the run list, without `plugin`. Its one tool-free
    /// turn is the evaluation's fixed cost.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_native_probe_child_declares_the_run_list() {
        let fixture = Fixture::start(
            "native-probe",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let started = std::time::Instant::now();
        let native = native_probe(&options, &CancellationToken::new())
            .await
            .unwrap();
        println!("native probe: {} ms", started.elapsed().as_millis());
        let names: Vec<&str> = native.iter().map(|(name, _)| name.as_str()).collect();
        for tool in RUN_TOOLS {
            assert!(
                names.contains(tool),
                "the native set declares {tool}: {names:?}"
            );
        }
        assert_eq!(
            names.len(),
            RUN_TOOLS.len(),
            "the native set is the run list: {names:?}"
        );
        assert!(
            !names.contains(&PLUGIN_TOOL),
            "the served process registers no plugin tool: {names:?}"
        );
    }

    /// The native probe starts from clean directories: a file left in the
    /// probe's root and home by an earlier evaluation is gone when the
    /// probe runs, so a reused --out cannot answer for this evaluation's
    /// native set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_native_probe_clears_its_directories() {
        let fixture = Fixture::start(
            "native-probe-clean",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let stale_root = options.out.join("native-probe").join("stale.json");
        let stale_home = options
            .out
            .join("homes")
            .join("native-probe")
            .join("stale.json");
        for path in [&stale_root, &stale_home] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "{}").unwrap();
        }
        let native = native_probe(&options, &CancellationToken::new())
            .await
            .unwrap();
        assert!(!native.is_empty(), "the probe declared its tool set");
        assert!(!stale_root.exists(), "the probe starts from a clean root");
        assert!(!stale_home.exists(), "the probe starts from a clean home");
    }

    /// A child's params for the checks that read only the tool sets: the
    /// native set, the defaults file, and the paths the other checks name;
    /// no serve child runs.
    fn products_params(defaults: PathBuf, native: &[&str]) -> ChildParams {
        ChildParams {
            home: PathBuf::from("/nonexistent/home"),
            workspace: PathBuf::from("/nonexistent/workspace"),
            env_home: PathBuf::from("/nonexistent/env-home"),
            output_root: PathBuf::from("/nonexistent/output"),
            socket: PathBuf::from("/nonexistent/socket"),
            model_config: PathBuf::from("/nonexistent/model.json"),
            defaults: Some(defaults),
            node: None,
            js_runtime: None,
            overrides: json!({}),
            scenario: json!({}),
            model: target("gateway/mock").unwrap(),
            judge: target("gateway/mock").unwrap(),
            boundary: None,
            outputs: PathBuf::from("/nonexistent/outputs"),
            credentials: Vec::new(),
            operator_home: None,
            isolate: false,
            fail_preflight: false,
            offer_plugin: false,
            extra_tool: false,
            fail_child: None,
            fail_child_holds_stdout: false,
            native_tools: native
                .iter()
                .map(|name| ((*name).to_owned(), ToolSource::Builtin))
                .collect(),
            probe_only: false,
        }
    }

    /// A contribution no named plugin owns fails `tools-product`: with an
    /// empty defaults file, the declared set is the native set plus one
    /// stray tool and no plugin is named to own it, so the check fails
    /// naming the tool; the same input passes once the defaults name a
    /// plugin, and the detail attributes the tool to it.
    #[test]
    fn a_contribution_no_named_plugin_owns_fails_the_check() {
        let directory = std::env::temp_dir().join(format!(
            "dim-eval-unowned-contribution-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let native = &RUN_TOOLS[..2];
        let mut declared: Vec<(String, ToolSource)> = native
            .iter()
            .map(|name| ((*name).to_owned(), ToolSource::Builtin))
            .collect();
        declared.push(("fixture_extra".to_owned(), ToolSource::Extension));
        let named: Vec<String> = declared.iter().map(|(name, _)| name.clone()).collect();
        let expected: Vec<String> = native.iter().map(|name| (*name).to_owned()).collect();
        let empty = directory.join("empty.json");
        std::fs::write(&empty, json!({"plugins": {}}).to_string()).unwrap();
        let params = products_params(empty, native);
        let checks = preflight_checks(&[], &params, &named, &named, &expected, &named, &declared);
        let product = check(&checks, "tools-product");
        assert!(!product.passed, "{product:?}");
        assert!(product.detail.contains("fixture_extra"), "{product:?}");
        assert!(product.detail.contains("nothing is exempt"), "{product:?}");
        let owned = directory.join("owned.json");
        std::fs::write(
            &owned,
            json!({"plugins": {"text-injector": {"path": "nowhere"}}}).to_string(),
        )
        .unwrap();
        let params = products_params(owned, native);
        let checks = preflight_checks(&[], &params, &named, &named, &expected, &named, &declared);
        let product = check(&checks, "tools-product");
        assert!(product.passed, "{product:?}");
        assert!(product.detail.contains("text-injector"), "{product:?}");
        assert!(
            product.detail.contains("fixture_extra (extension)"),
            "{product:?}"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A defaults file exempts nothing by its presence: with an empty
    /// defaults (`{"plugins":{}}`) the run's declared set equals the native
    /// set, so a harness-contributed native tool the run list does not name
    /// still fails `tools-product` — the case a source-based exemption let
    /// through. The run is unisolated, so the failed check is recorded and
    /// the run proceeds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
    async fn node_a_run_with_empty_defaults_rejects_a_native_tool() {
        let fixture = Fixture::start(
            "plugin-empty-defaults",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let defaults = fixture.directory.join("empty-defaults.json");
        std::fs::write(&defaults, json!({"plugins": {}}).to_string()).unwrap();
        let mut options = fixture.plugin_options(fixture.directory.join("out"), defaults);
        options.isolate = false;
        options.extra_tool = true;
        let _ = fixture.evaluate(&options).await.unwrap();
        let checks = recorded_checks(&options.out, "current");
        let product = check(&checks, "tools-product");
        assert!(!product.passed, "{product:?}");
        assert!(product.detail.contains("fixture_extra"), "{product:?}");
    }

    /// A run's plugin defaults are attributed by the differential probe, not
    /// by a tool's source: with the text-injector defaults, the plugin's
    /// `fixture_prompt` is accepted as the defaults' contribution, and the
    /// harness's native `fixture_extra` — the same Extension source — is
    /// refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
    async fn node_a_run_with_plugin_defaults_refuses_an_unattributed_tool() {
        let fixture = Fixture::start(
            "plugin-defaults-extra",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let defaults = fixture.directory.join("plugin-defaults.json");
        std::fs::write(
            &defaults,
            json!({"plugins": {"text-injector": repository().join("runtime/fixtures/text-injector")}})
                .to_string(),
        )
        .unwrap();
        let mut options = fixture.plugin_options(fixture.directory.join("out"), defaults);
        options.isolate = false;
        options.extra_tool = true;
        let _ = fixture.evaluate(&options).await.unwrap();
        let checks = recorded_checks(&options.out, "current");
        let product = check(&checks, "tools-product");
        assert!(!product.passed, "{product:?}");
        assert!(product.detail.contains("fixture_extra"), "{product:?}");
        assert!(
            product.detail.contains("fixture_prompt (extension)"),
            "the defaults' contribution is accepted: {product:?}"
        );
    }

    /// A failed run cancels its in-flight siblings before `evaluate`
    /// returns: the cancelled run's serve child, and the process group it
    /// leads, is gone. The child drives the model and writes under `--out`,
    /// so it must not outlive the harness.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_run_settles_its_in_flight_sibling() {
        /// Holds the candidate arm's model calls until the failure cancels
        /// it; the current arm's child never reaches a model call.
        fn hold_the_candidate(text: &str) -> Option<Duration> {
            text.contains("/candidate/")
                .then(|| Duration::from_secs(60))
        }
        let fixture = Fixture::holding(
            "failed-sibling",
            reads_the_workspace_file,
            hold_the_candidate,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
            24_000,
        )
        .await;
        let directory = fixture.directory.join("out");
        let mut options = fixture.options(directory.clone());
        options.jobs = 2;
        // The failing child waits for the event the assertions read, not
        // for a fixed time: the marker is written as soon as the fixture
        // provider records the candidate arm's model call, which it does
        // just before it holds the answer.
        let marker = fixture.directory.join("held.marker");
        let _watcher = {
            let provider = fixture.provider.clone();
            let marker = marker.clone();
            tokio::spawn(async move {
                loop {
                    let held = provider
                        .requests
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|request| request.contains("/candidate/"));
                    if held {
                        std::fs::write(&marker, "the candidate arm's call is held").unwrap();
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
        };
        options.fail_child = Some(("current".into(), marker));
        let settled = fixture.evaluate(&options).await;
        let error = settled
            .expect_err("the failing run fails the evaluation")
            .to_string();
        // The failing child saw the event this test waited for; a missed
        // event fails here with the child's own message instead of passing
        // because the sibling happened not to be working yet.
        assert!(
            error.contains("the held sibling's model call was seen"),
            "the sibling's model call was held: {error}"
        );
        // Its child, and the group it leads, is gone.
        let params = directory.join("children/candidate/case/run-0.json");
        let pid = SPAWNED
            .lock()
            .unwrap()
            .iter()
            .find(|(path, _)| path == &params)
            .map(|(_, pid)| *pid)
            .expect("the sibling's serve child was spawned");
        assert!(
            child_tree_gone(pid).await,
            "the sibling's process and its group are gone (pid {pid})"
        );
    }

    /// A child that fails after it started a member of its group does not
    /// leave it behind: the group the child leads is killed on the non-zero
    /// exit, so the member is gone when `evaluate` returns. The failing
    /// child records its descendant's pid beside the marker before it
    /// exits; the descendant is a `sleep` that joins the child's group (it
    /// starts no session of its own).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_child_kills_the_group_it_leaves() {
        let fixture = Fixture::start(
            "failed-descendant",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let directory = fixture.directory.join("out");
        let mut options = fixture.options(directory);
        // The marker is already there: the failing child needs no held
        // sibling, only its own non-zero exit path.
        let marker = fixture.directory.join("fail.marker");
        std::fs::write(&marker, "go").unwrap();
        options.fail_child = Some(("current".into(), marker.clone()));
        let settled = fixture.evaluate(&options).await;
        let error = settled
            .expect_err("the failing run fails the evaluation")
            .to_string();
        assert!(
            error.contains("the held sibling's model call was seen"),
            "the failing child saw its marker: {error}"
        );
        let pid: u32 = std::fs::read_to_string(marker.with_extension("descendant"))
            .expect("the failing child recorded its descendant")
            .trim()
            .parse()
            .expect("the descendant's pid");
        assert!(
            child_tree_gone(pid).await,
            "the failed child's descendant and its group are gone (pid {pid})"
        );
    }

    /// A failed child's descendant that inherits the child's standard
    /// output must not keep `evaluate` from the run's error: the group is
    /// killed as soon as the non-zero status is known, before the parent
    /// reads the child's output, so the read reaches EOF. The bound makes
    /// an ordering regression a failed assertion instead of a hang.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_child_holding_stdout_returns_its_error() {
        let fixture = Fixture::start(
            "failed-stdout-holder",
            reads_the_workspace_file,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let directory = fixture.directory.join("out");
        let mut options = fixture.options(directory);
        // The marker is already there: the failing child needs no held
        // sibling, and its descendant inherits its standard output.
        let marker = fixture.directory.join("fail.marker");
        std::fs::write(&marker, "go").unwrap();
        options.fail_child = Some(("current".into(), marker.clone()));
        options.fail_child_holds_stdout = true;
        let settled = tokio::time::timeout(Duration::from_secs(10), fixture.evaluate(&options))
            .await
            .expect("the evaluation returns within the bound");
        let error = settled
            .expect_err("the failing run fails the evaluation")
            .to_string();
        assert!(
            error.contains("the held sibling's model call was seen"),
            "the failing child saw its marker: {error}"
        );
        let pid: u32 = std::fs::read_to_string(marker.with_extension("descendant"))
            .expect("the failing child recorded its descendant")
            .trim()
            .parse()
            .expect("the descendant's pid");
        assert!(
            child_tree_gone(pid).await,
            "the failed child's stdout holder and its group are gone (pid {pid})"
        );
    }

    /// Whether a serve child and the process group it leads have no member
    /// left, probed for up to five seconds: the harness kills the group, and
    /// the kill and the reaping of its leader settle a moment later.
    #[cfg(unix)]
    async fn child_tree_gone(pid: u32) -> bool {
        for _ in 0..250 {
            // SAFETY: signal 0 only probes the process and its group.
            let process = unsafe { libc::kill(pid as libc::pid_t, 0) };
            let process_gone = process == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            let group = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
            let group_gone =
                group == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if process_gone && group_gone {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// A model-created collaboration member cannot mount the `plugin`
    /// management tool: the served process does not register it, so the
    /// member's own model request declares the profile tools that exist
    /// (`read`) and not `plugin` (issue #1325). The scripted model asks the
    /// `agent` tool for a member whose inline profile names `plugin`; the
    /// member's request is the only one that declares `read` without the
    /// run's whole set (`exec`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_isolated_run_member_declares_no_plugin_tool() {
        fn delegates_to_a_member(text: &str) -> Option<Value> {
            judged(text)
                .or_else(|| {
                    text.contains("You summarize a coding session")
                        .then(|| content("SUMMARY OF EARLIER TURNS"))
                })
                .or_else(|| {
                    // The member's own request (its profile's prompt), and
                    // the preflight and probe Sessions: answer in text.
                    (!text.contains("Run the probes.")).then(|| content("the member answered"))
                })
                .or_else(|| {
                    // The leader's first turn: delegate once, then answer.
                    (!text.contains("\\\"role\\\":\\\"tool\\\"")).then(|| {
                        tool_call(
                            "agent",
                            r#"{"action":"request","profile":{"name":"helper","description":"","systemPrompt":"work","tools":["plugin","read"],"model":{"providerId":"gateway","modelId":"mock"}},"prompt":"work"}"#,
                        )
                    })
                })
        }
        let fixture = Fixture::start(
            "isolated-member",
            delegates_to_a_member,
            isolated_scenario(),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate is inconclusive"), "{report}");
        let requests = fixture.provider.requests.lock().unwrap().clone();
        let member = requests
            .iter()
            .find(|request| {
                let tools = offered_tools(request);
                tools.iter().any(|tool| tool == "read") && !tools.iter().any(|tool| tool == "exec")
            })
            .expect("the member's model request");
        let offered = offered_tools(member);
        assert!(
            offered.iter().any(|tool| tool == "read"),
            "the member declares the tools its profile names: {offered:?}"
        );
        assert!(
            !offered.iter().any(|tool| tool == "plugin"),
            "the member declares no plugin tool: {offered:?}"
        );
    }
}

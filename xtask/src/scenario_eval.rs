//! `scenario-eval`: the scenario runner for policy texts (model-prompts §8.1,
//! §8.2 and §9.8; Timeline branches §12.3). It calls real models, so it is not
//! part of CI: the operator runs it against an isolated product home or a
//! model configuration, and the output goes outside the repository.
//!
//! ```text
//! cargo run --locked -p xtask -- scenario-eval --scenarios DIR --text ID
//!     --candidate FILE --model PROVIDER/MODEL --judge PROVIDER/MODEL --runs N
//!     --home DIR --out DIR [--model-config FILE]
//! ```
//!
//! Both arms run the shipped assembly: `current` with the product's own texts
//! and `candidate` with the candidate file in place of the text `--text`
//! names. Each arm runs every scenario `--runs` times, one fixed judge reads
//! a view of each run's exported Timeline, and the report applies the
//! relative rule of model-prompts §8.1: every probe's pass rate is at least
//! the current arm's minus 0.05, the candidate's violations, failed runs and
//! unjudged runs are no higher, and every probe has at least one judged run
//! in each arm. There is no absolute pass line.
//!
//! A failed run is one that ended on an error or its wall-clock budget, or
//! one whose judging did not happen: there was nothing the judge could be
//! asked, or a judge call failed — the first call, its retry without a
//! temperature, or a re-ask. The judge is asked for `present` or `absent`;
//! the verdict parser it shares with the summary evaluation also accepts
//! `contradicted`, which a `must_not_show` probe treats as not shown. The
//! judge's verdicts are read from a JSON array or from one object per line; a
//! reply that gives no verdict, or leaves a statement without one, is asked
//! again with what was wrong, at most twice.
//! A run the judge answered but still gave no usable verdicts for after the
//! re-asks is unjudged, and so is a run whose view does not fit the judge's
//! budget, which is never asked: either adds nothing to its arm's probe
//! rates, violations or failed runs, and the report counts it and lists it
//! with its reason. A run that failed on its own is a failed run, scored as
//! before, whatever its judge did.
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
//! rows need Node and the `@arcships/rutis-runtime` packages: the runner
//! reads `DIM_TEST_NODE` and `DIM_TEST_JS_RUNTIME` from the environment, and
//! each default plugin's packages must be installed (`npm ci --prefix
//! <plugin directory>`). For example:
//!
//! ```text
//! DIM_TEST_NODE="$(command -v node)" \
//! DIM_TEST_JS_RUNTIME="$PWD/runtime/node_modules/@arcships/rutis-runtime" \
//! cargo run --locked -p xtask -- scenario-eval ...
//! ```
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
//! - `message` is the operator's first message, submitted to `main`. A steer
//!   is one more operator message to `main`, sent when the run reaches
//!   `afterSeconds` of wall clock or, for `whenToolCall`, when a
//!   `step/tool-use` event carries the substring. A steer sets exactly one of
//!   the two. A question the run asks is declined: the scenario scripts no
//!   answer to it.
//! - `budget.turns` bounds the turns over every line and `budget.seconds` the
//!   wall clock from the first submission; both stop the run. `budget.window`
//!   is the context window the scenario is written for and is checked, not
//!   set: the window is the model's declared limit, so a run whose Session
//!   declares another one is a failed run.
//! - A `must_show` probe passes when the judge finds that the run shows it; a
//!   `must_not_show` probe passes when the run does not, and a run that shows
//!   it is a violation.
//!
//! The runner copies the scenario's workspace and `--home` into a fresh
//! directory for every run, so runs share no state, and writes one directory
//! per run under `--out`, which it creates and resolves where the command
//! line is read: `timeline.jsonl` (the whole Timeline, every line included:
//! the run's account of record), `judge-view.jsonl` (the view of it the judge
//! read), `home/` and `workspace/`. The App
//! Server's local endpoint sits under the system temporary directory, because
//! a Unix socket's path is short and a run's directory is not; each run
//! owns its endpoint directory and removes it when the run ends, on success
//! and on error, without affecting other runs.
use crate::app_server::{self, Overrides, RunSettings};
use crate::summary_eval::{Configuration, Judgement, Verdict, complete, parse_judgements};
use dim_agent_capabilities::{
    context_model::window_facts,
    host::{ModelHost, ModelSelectionHost, ProviderConnectionResolver, ResolveContextModel},
};
use dim_agent_runtime::AgentBuilder;
use dim_agent_types::{
    Message, ModelConfig, ModelId, ModelRequest, ModelSelection, Role, SystemPrompt,
};
use dim_app_server::RunningServer;
use dim_client::{Client, ConnectionConfig, Message as WireMessage, Server as WireServer};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The rule's δ (model-prompts §8.1): relative, with no absolute pass line.
/// Held as a fraction so the boundary is compared in integers; as `f64` rates
/// some run counts fail a candidate that is exactly at `current − δ`.
const DELTA: (u64, u64) = (1, 20);
/// δ as the report prints it.
fn delta() -> f64 {
    DELTA.0 as f64 / DELTA.1 as f64
}
/// How long the driver waits after the last event before it asks whether
/// every line is idle. The quiet period follows the last notice the driver
/// reads, including the notices it reads while it waited on `session.idle`.
const QUIET: Duration = Duration::from_secs(1);
/// How long one `session.idle` call may take before its line counts as busy.
const IDLE_WAIT: Duration = Duration::from_secs(1);

const JUDGE_SYSTEM: &str = "You check statements against the exported Timeline of a coding session. The Timeline is JSONL: a header line, one branch line per line of the Session, one event line per event, and an end line. A context/prepare or model/call event carries its request: addedMessages are the messages added since that line's previous request, with messageCount the whole request's count, and messages is the whole list when the history changed; every other field of the request appears in full only when it changed, and otherwise reads {\"unchanged\": true}. For each statement answer \"present\" when the run shows it and \"absent\" when it does not. Add the Timeline events that support the verdict as a short quote. Answer with a JSON array only, one object per statement: {\"id\": \"<statement id>\", \"verdict\": \"present|absent\", \"quote\": \"<Timeline evidence>\"}.";

/// The message that asks the judge again, `{what}` saying why its last answer
/// could not be used.
const JUDGE_REASK: &str = "Your answer could not be used: {what}. Answer again with the JSON array only, one object per statement.";
/// How many times one run's judge is asked again for an answer it cannot use.
const REASKS: u32 = 2;

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
    Plugin {
        plugin: &'static str,
        member: &'static str,
    },
}
/// The policy texts a candidate can replace, and the factory configuration
/// member each is put in place of in the assembly [`app_server::serve_home`]
/// builds. The summary prompt (ContextCompaction §13.5) and the branch tool's
/// description (timeline branches §12.4) are the texts exposed at a Rust
/// factory configuration today; the other owners add their entries with their
/// own factory members: Capabilities under
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
        "goal/objective",
        TextMember::Plugin {
            plugin: "goal",
            member: "texts.objective",
        },
    ),
    (
        "goal/continuation",
        TextMember::Plugin {
            plugin: "goal",
            member: "texts.continuation",
        },
    ),
];

pub async fn run() -> Result<()> {
    let options = Options::parse()?;
    let report = evaluate(&options, |_| {}).await?;
    println!("{report}");
    println!("wrote {}", options.out.join("scenario.md").display());
    Ok(())
}

/// Runs the evaluation and writes `runs.jsonl` and `scenario.md`; returns the
/// report. `observe` sees every run's App Server builder before it is served,
/// the seam [`app_server::serve_home`] gives a caller that places its own
/// values; the flow tests point a run's process facts at a home of their own
/// instead of the developer's.
async fn evaluate(options: &Options, observe: impl Fn(&mut AgentBuilder)) -> Result<String> {
    if options.defaults.is_some() {
        if options.node.is_none() {
            return Err("set DIM_TEST_NODE: the default plugins run in Node".into());
        }
        if options.js_runtime.is_none() {
            return Err("set DIM_TEST_JS_RUNTIME: the default plugins import it".into());
        }
    }
    let overrides = overrides(&options.text, options.candidate.clone())?;
    let scenarios = read_scenarios(&options.scenarios)?;
    let home = isolated_home(&options.home)?;
    if let Some(path) = &options.model_config {
        dim_app_server::cli::model_profile(path)
            .map_err(|error| format!("--model-config {}: {error}", path.display()))?;
    }
    let cancel = CancellationToken::new();
    let mut records = Vec::new();
    let mut instrument = Instrument {
        text: options.text.clone(),
        model: options.model.clone(),
        judge: options.judge.clone(),
        runs: options.runs,
        tokens: text_tokens(&options.model, &options.candidate),
        model_configuration: None,
        judge_configuration: None,
        judge_window: None,
        judge_temperature_dropped: 0,
        judge_asked_runs: 0,
        judge_reasks: 0,
    };
    for (arm, overrides) in [("current", Overrides::default()), ("candidate", overrides)] {
        for scenario in &scenarios {
            for run in 0..options.runs {
                let record = run_once(
                    options,
                    &home,
                    arm,
                    &overrides,
                    &observe,
                    scenario,
                    run,
                    &cancel,
                    &mut instrument,
                )
                .await?;
                instrument.judge_temperature_dropped += usize::from(record.temperature_dropped);
                instrument.judge_asked_runs += usize::from(!matches!(
                    record.judge_outcome,
                    JudgeOutcome::NotAsked | JudgeOutcome::Unfit
                ));
                instrument.judge_reasks += record.judge_reasks as usize;
                records.push(record);
            }
        }
    }
    let mut jsonl = String::new();
    for record in &records {
        jsonl += &serde_json::to_string(record)?;
        jsonl.push('\n');
    }
    std::fs::write(options.out.join("runs.jsonl"), jsonl)?;
    let report = report(&instrument, &scenarios, &records);
    std::fs::write(options.out.join("scenario.md"), &report)?;
    Ok(report)
}

struct Options {
    scenarios: PathBuf,
    text: String,
    candidate: Arc<str>,
    model: ModelId,
    judge: ModelId,
    runs: usize,
    out: PathBuf,
    home: PathBuf,
    model_config: Option<PathBuf>,
    /// The default plugin configuration file both arms load; `None` serves
    /// the assembly without the product's plugins.
    defaults: Option<PathBuf>,
    node: Option<PathBuf>,
    js_runtime: Option<PathBuf>,
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
                     --text ID --candidate FILE --model PROVIDER/MODEL \
                     --judge PROVIDER/MODEL --runs N --home DIR --out DIR \
                     [--model-config FILE] [--defaults FILE]; a run whose \
                     view does not fit the judge's own declared window is \
                     unjudged, so a model configuration narrowed for the \
                     scenarios declares the judge under its own provider \
                     entry (see the module doc)";
        // Both arms serve the product's default plugins; `--defaults`
        // names another default configuration file.
        let mut args = args.into_iter();
        let (mut scenarios, mut home, mut out) = (None, None, None);
        let (mut text, mut candidate, mut model, mut judge) = (None, None, None, None);
        let (mut runs, mut model_config, mut defaults) = (None, None, None);
        while let Some(flag) = args.next() {
            let value = args.next().ok_or(format!("{flag} requires a value"))?;
            match flag.as_str() {
                "--scenarios" => scenarios = Some(PathBuf::from(value)),
                "--text" => text = Some(value),
                "--candidate" => candidate = Some(candidate_prompt(Path::new(&value))?),
                "--model" => model = Some(target(&value)?),
                "--judge" => judge = Some(target(&value)?),
                "--runs" => runs = Some(value.parse::<usize>()?),
                "--home" => home = Some(PathBuf::from(value)),
                "--out" => out = Some(PathBuf::from(value)),
                "--model-config" => model_config = Some(PathBuf::from(value)),
                "--defaults" => defaults = Some(PathBuf::from(value)),
                _ => return Err(usage.into()),
            }
        }
        let defaults = Some(defaults.unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime/default-plugins.json")
        }));
        Ok(Self {
            scenarios: scenarios.ok_or(usage)?,
            text: text.ok_or(usage)?,
            candidate: candidate.ok_or(usage)?,
            model: model.ok_or(usage)?,
            judge: judge.ok_or(usage)?,
            runs: runs.filter(|runs| *runs > 0).ok_or(usage)?,
            out: output_directory(&out.ok_or(usage)?)?,
            home: home.ok_or(usage)?,
            model_config,
            defaults,
            node: env_path("DIM_TEST_NODE"),
            js_runtime: env_path("DIM_TEST_JS_RUNTIME"),
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

/// Creates `--out` and resolves it: `session.create` takes an absolute cwd,
/// and every run path is built from this one.
fn output_directory(path: &Path) -> Result<PathBuf> {
    let error = |error: std::io::Error| format!("--out {}: {error}", path.display());
    std::fs::create_dir_all(path).map_err(error)?;
    Ok(std::fs::canonicalize(path).map_err(error)?)
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

/// The overrides that put `candidate` in place of the text `text` names in
/// [`TEXTS`]; an unknown id fails before any run and lists the known ids.
fn overrides(text: &str, candidate: Arc<str>) -> Result<Overrides> {
    overrides_in(TEXTS, text, candidate)
}

/// [`overrides`] over `table`: a test reaches a member no shipped text
/// exposes yet with its own entry, through the same injection.
fn overrides_in(
    table: &[(&str, TextMember)],
    text: &str,
    candidate: Arc<str>,
) -> Result<Overrides> {
    let Some((_, member)) = table.iter().find(|(id, _)| *id == text) else {
        let known: Vec<_> = table.iter().map(|(id, _)| *id).collect();
        return Err(format!(
            "--text {text}: unknown text id; known ids: {}",
            known.join(", ")
        )
        .into());
    };
    let mut overrides = Overrides::default();
    match member {
        TextMember::Rust(apply) => apply(&mut overrides, candidate),
        TextMember::Plugin { plugin, member } => {
            let mut value = Value::String(candidate.to_string());
            for key in member.split('.').rev() {
                value = Value::Object([(key.to_owned(), value)].into_iter().collect());
            }
            overrides.plugin_configs.insert((*plugin).to_owned(), value);
        }
    }
    Ok(overrides)
}

/// `--home` must be an isolated product home; it must never be the product
/// home. Every run copies it, so a run never shares state with another.
fn isolated_home(path: &Path) -> Result<PathBuf> {
    let home = std::fs::canonicalize(path)
        .map_err(|error| format!("--home {}: {error}", path.display()))?;
    let mut products = Vec::new();
    if let Some(user) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        products.push(PathBuf::from(&user).join(".dimcode-alpha"));
        products.push(PathBuf::from(user).join(".dimcode"));
    }
    if let Some(path) = std::env::var_os("DIMCODE_HOME") {
        products.push(PathBuf::from(path));
    }
    for product in products {
        if std::fs::canonicalize(product).ok().as_deref() == Some(home.as_path()) {
            return Err("--home must be an isolated product home, never the product home".into());
        }
    }
    Ok(home)
}

/// The candidate text's own token cost, counted with the product's counter
/// (`window_facts().count_tokens`) on the request the text travels in. The
/// count reads the request's bytes; the window it is built with does not
/// enter it.
pub(crate) fn text_tokens(model: &ModelId, text: &str) -> f64 {
    let mut request = ModelRequest::new(&model.provider_id, &model.model_id, vec![]);
    request.system = Some(Arc::new(SystemPrompt::Text(text.into())));
    window_facts(0, 0, &request).count_tokens(&request)
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
    #[serde(default)]
    steers: Vec<Steer>,
    budget: Budget,
    probes: Vec<Probe>,
}
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Call {
    method: String,
    /// The method's parameters without the Session id, which the runner
    /// resolves.
    #[serde(default)]
    params: Value,
}
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Steer {
    #[serde(default)]
    after_seconds: Option<f64>,
    #[serde(default)]
    when_tool_call: Option<String>,
    text: String,
}
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Budget {
    turns: u32,
    seconds: f64,
    window: u64,
}
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Probe {
    id: String,
    kind: ProbeKind,
    text: String,
}
#[derive(Clone, Copy, serde::Deserialize, PartialEq, Eq)]
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
#[derive(Clone)]
struct Scenario {
    id: String,
    description: String,
    directory: PathBuf,
    message: String,
    calls: Vec<Call>,
    steers: Vec<Steer>,
    budget: Budget,
    probes: Vec<Probe>,
}

fn read_scenarios(directory: &Path) -> Result<Vec<Scenario>> {
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
    raw: ScenarioFile,
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
    #[serde(skip)]
    temperature_dropped: bool,
    probes: Vec<ProbeRecord>,
}
impl RunRecord {
    /// Whether the run is unjudged (model-prompts §8.1): the judge was not
    /// asked because the view of the run's export does not fit its budget, or
    /// it answered without usable verdicts after the re-asks, and nothing
    /// else failed the run. Such a run enters none of its arm's metrics.
    fn unjudged(&self) -> bool {
        !self.failed
            && matches!(
                self.judge_outcome,
                JudgeOutcome::Unreadable | JudgeOutcome::Unfit
            )
    }
    /// Whether the run is judged (model-prompts §8.1): it raised no error and
    /// the judge answered every call with usable verdicts. Only such a run
    /// counts toward its arm's and each of its probes' judged runs.
    fn judged(&self) -> bool {
        self.error.is_none() && self.judge_outcome == JudgeOutcome::Judged
    }
}
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeRecord {
    id: String,
    kind: &'static str,
    verdict: ProbeVerdict,
    quote: Option<String>,
}
#[derive(Clone, Copy, serde::Serialize, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum JudgeOutcome {
    /// The judge answered every call, and its verdicts are usable.
    Judged,
    /// A judge call failed: a window overflow, a transport or an HTTP error,
    /// on the first call, on its retry, or on a re-ask.
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

#[allow(clippy::too_many_arguments)]
async fn run_once(
    options: &Options,
    home: &Path,
    arm: &'static str,
    overrides: &Overrides,
    observe: &dyn Fn(&mut AgentBuilder),
    scenario: &Scenario,
    run: usize,
    cancel: &CancellationToken,
    instrument: &mut Instrument,
) -> Result<RunRecord> {
    let run_directory = options
        .out
        .join(arm)
        .join(&scenario.id)
        .join(format!("run-{run}"));
    if run_directory.exists() {
        return Err(format!(
            "{} exists; give a fresh --out for a new evaluation",
            run_directory.display()
        )
        .into());
    }
    private_directory(&run_directory)?;
    let run_home = run_directory.join("home");
    copy_tree(home, &run_home)?;
    let workspace = run_directory.join("workspace");
    copy_tree(&scenario.directory.join("workspace"), &workspace)?;
    let sockets = Sockets::new()?;
    let settings = RunSettings {
        overrides: overrides.clone(),
        model_config: options.model_config.clone(),
        unix_path: Some(sockets.0.join("rpc.sock")),
        defaults: options.defaults.clone(),
        node: options.node.clone(),
        js_runtime: options.js_runtime.clone(),
    };
    let server = app_server::serve_home(
        &run_home,
        &workspace,
        options.out.join("command-output"),
        settings.clone(),
        |builder| observe(builder),
    )
    .await?;
    let started = Instant::now();
    let model = selection(&server, &options.model, &workspace, cancel).await?;
    let judge = selection(&server, &options.judge, &workspace, cancel).await?;
    instrument
        .model_configuration
        .get_or_insert_with(|| Configuration::read(model.config.as_ref()));
    instrument
        .judge_configuration
        .get_or_insert_with(|| Configuration::read(judge.config.as_ref()));
    let limits = judge_window(&judge, options.model_config.as_deref(), cancel).await?;
    instrument.judge_window.get_or_insert(limits);
    let finished = match drive_once(
        &server, &settings, &run_home, &workspace, scenario, &model, &judge, limits, cancel,
    )
    .await
    {
        Ok(finished) => finished,
        Err(error) => Finished::failed(scenario, error.to_string()),
    };
    server.shutdown();
    server.join().await?;
    if let Some(export) = &finished.export {
        std::fs::write(run_directory.join("timeline.jsonl"), export)?;
    }
    if let Some(view) = &finished.view {
        std::fs::write(run_directory.join("judge-view.jsonl"), view)?;
    }
    let driven = finished.driven;
    let error = driven.error;
    Ok(RunRecord {
        arm,
        scenario: scenario.id.clone(),
        run,
        session: (!driven.session.is_empty()).then_some(driven.session),
        stop: driven.stop,
        turns: driven.turns,
        window: driven.window,
        duration_ms: started.elapsed().as_secs_f64() * 1000.,
        failed: failed(driven.stop, error.as_deref(), finished.judging.outcome),
        judge_outcome: finished.judging.outcome,
        error,
        judge_error: finished.judging.error,
        judge_reasks: finished.judging.reasks,
        temperature_dropped: finished.judging.temperature_dropped,
        probes: finished.judging.probes,
    })
}

/// What one run left behind: the driven Session, its export, the view of it
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
    })
    .await
    .map_err(|error| format!("cannot connect to the served App Server: {error}"))?;
    if settings.defaults.is_some() {
        wait_for_plugins(&mut client).await?;
    }
    let mut driven = drive(&mut client, scenario, workspace, model).await;
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

async fn selection(
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
    error: Option<String>,
}

/// Drives one Session through the App Server's client methods, as a client
/// does: create, attach, submit the script, and wait for idle or a budget.
async fn drive(
    client: &mut Client,
    scenario: &Scenario,
    workspace: &Path,
    model: &ModelSelection,
) -> Driven {
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": workspace,
                "model": {"provider": model.provider_id, "modelId": model.model_id},
                "permissionPolicy": {"preset": "full-access"},
            }),
        )
        .await;
    let session = match created {
        Ok(reply) => reply["sessionId"].as_str().unwrap_or_default().to_owned(),
        Err(error) => {
            return Driven {
                session: String::new(),
                stop: "error",
                turns: 0,
                window: None,
                error: Some(format!("session.create failed: {error}")),
            };
        }
    };
    let mut driven = Driven {
        session: session.clone(),
        stop: "idle",
        turns: 0,
        window: None,
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
            if window != Some(scenario.budget.window as f64) {
                driven.stop = "error";
                driven.error = Some(format!(
                    "the Session declares a context window of {}, and the scenario declares {}",
                    window.map_or_else(|| "none".to_owned(), |window| window.to_string()),
                    scenario.budget.window
                ));
                return driven;
            }
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
    if let Err(error) = submit(client, &session, &scenario.message).await {
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
    // An input the runner submitted is outstanding until its turn starts; a
    // line that has not started it yet is not idle.
    let mut awaiting = true;
    loop {
        match fire_due(client, &session, &mut steers, started).await {
            Ok(submitted) => awaiting |= submitted,
            Err(error) => {
                driven.stop = "error";
                driven.error = Some(error);
                return driven;
            }
        }
        let pending = steers.iter().any(|(_, fired)| !fired);
        if started.elapsed().as_secs_f64() >= scenario.budget.seconds {
            driven.stop = "seconds";
            break;
        }
        if driven.turns >= scenario.budget.turns {
            driven.stop = "turns";
            break;
        }
        if !pending && !awaiting && last_event.elapsed() >= QUIET {
            match idle(client, &session, &branches).await {
                Ok(true) => break,
                Ok(false) => last_event = Instant::now(),
                Err(error) => {
                    driven.stop = "error";
                    driven.error = Some(error);
                    return driven;
                }
            }
        }
        let wait = wait_time(&steers, started, &scenario.budget);
        match tokio::time::timeout(wait, client.next()).await {
            Ok(Ok(WireMessage::Notice { method, params })) => {
                last_event = Instant::now();
                match notice(&method, &params) {
                    Notice::Event { branch, event } => {
                        if event["type"] == "branch"
                            && event["name"] == "create"
                            && let Some(branch) = event["data"]["branch"].as_str()
                        {
                            branches.insert(branch.to_owned());
                        }
                        if event["type"] == "turn" && event["name"] == "start" {
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
                        decline_question(client, &session, &branch, &event).await;
                    }
                    Notice::Approval { branch, request } => {
                        let _ = client
                            .rpc(
                                "approval.respond",
                                json!({"sessionId": session, "branchId": branch,
                                       "requestId": request, "decision": {"kind": "allow"}}),
                            )
                            .await;
                    }
                    Notice::Other => {}
                }
            }
            Ok(Ok(_)) => {}
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
    driven
}

/// A question the run asks is declined: the scenario scripts no answer to it.
async fn decline_question(client: &mut Client, session: &str, branch: &str, event: &Value) {
    if event["type"] != "step" || event["name"] != "tool-call" {
        return;
    }
    let request = event["data"]["request_id"].as_str().unwrap_or_default();
    let Some(calls) = event["data"]["calls"].as_array() else {
        return;
    };
    for call in calls {
        if call["name"] != "ask_user" {
            continue;
        }
        let _ = client
            .rpc(
                "session.question.respond",
                json!({"sessionId": session, "branchId": branch, "requestId": request,
                       "callId": call["id"], "declined": true}),
            )
            .await;
    }
}

async fn submit(client: &mut Client, session: &str, text: &str) -> std::result::Result<(), String> {
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": text}),
        )
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
            submit(client, session, &steer.text).await?;
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
            submit(client, session, &steer.text).await?;
            sent = true;
        }
    }
    Ok(sent)
}

/// How long to wait for the next notice: until the next timed steer, the
/// quiet check or the wall-clock budget, whichever comes first.
fn wait_time(steers: &[(Steer, bool)], started: Instant, budget: &Budget) -> Duration {
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

/// Whether every known line is idle and no notice arrived while it asked.
/// `session.idle` returns when the line is idle; a short deadline keeps a busy
/// line from holding the driver. A notice that arrives while a call waits is
/// left in the client's queue, and the driver reads every queued notice before
/// its next quiet check, so `false` here means the quiet period follows that
/// notice's read.
async fn idle(
    client: &mut Client,
    session: &str,
    branches: &BTreeSet<String>,
) -> std::result::Result<bool, String> {
    for branch in branches {
        let call = client.rpc(
            "session.idle",
            json!({"sessionId": session, "branchId": branch}),
        );
        match tokio::time::timeout(IDLE_WAIT, call).await {
            // An ended line has no turn to wait for.
            Ok(Ok(_)) | Ok(Err(_)) => {}
            Err(_) => return Ok(false),
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
    let whole = judge_request(judge, &messages, &config);
    let facts = window_facts(limits.window, limits.max_output, &whole);
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
            temperature_dropped: false,
        };
    }
    let (outcome, probes, error) = loop {
        let request = Arc::new(judge_request(judge, &messages, &config));
        let reply = match complete(host, resolver, &request, cancel).await {
            Ok(reply) => reply,
            // A call that fails during a re-ask ends the asking, and the
            // error names the call that failed.
            Err(error) if reasks > 0 => {
                break (
                    JudgeOutcome::CallFailed,
                    unparsed(scenario),
                    Some(format!("the judge failed on a re-ask: {error}")),
                );
            }
            Err(error) => {
                // The first call is retried once without a temperature.
                config.temperature = None;
                dropped = true;
                let retry = Arc::new(judge_request(judge, &messages, &config));
                match complete(host, resolver, &retry, cancel).await {
                    Ok(reply) => reply,
                    Err(retry) => {
                        break (
                            JudgeOutcome::CallFailed,
                            unparsed(scenario),
                            Some(format!("the judge failed: {error}, then {retry}")),
                        );
                    }
                }
            }
        };
        let what = match verdicts(scenario, &reply) {
            Ok(probes) => break (JudgeOutcome::Judged, probes, None),
            Err(what) if reasks == REASKS => {
                break (
                    JudgeOutcome::Unreadable,
                    unparsed(scenario),
                    Some(format!(
                        "the judge answered with no usable verdicts: {what}\n{reply}"
                    )),
                );
            }
            Err(what) => what,
        };
        reasks += 1;
        messages.push(Message::text(Role::Assistant, reply));
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

/// One probe's runs in one arm: every run that counts toward its pass rate,
/// and how many of them are judged.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ProbeCounts {
    passed: u32,
    runs: u32,
    judged: u32,
}

/// The arm's metrics over its scenarios and runs (model-prompts §8.1). An
/// unjudged run adds nothing to them; the arm counts it apart.
#[derive(Default)]
struct Metrics {
    /// `(scenario, probe)` → its counted and judged runs.
    probes: BTreeMap<(String, String), ProbeCounts>,
    violations: u32,
    failed: u32,
    /// Runs whose probes a judge answered.
    judged: u32,
    /// Runs the judge gave no answer to, that did not fail on their own,
    /// left out of every other count.
    unjudged: u32,
}
impl Metrics {
    fn add(&mut self, scenario: &Scenario, record: &RunRecord) {
        if record.unjudged() {
            self.unjudged += 1;
            return;
        }
        self.failed += u32::from(record.failed);
        let judged = record.judged();
        self.judged += u32::from(judged);
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
            if probe.kind == ProbeKind::MustNotShow && verdict == Some(ProbeVerdict::Present) {
                self.violations += 1;
            }
            let counts = self
                .probes
                .entry((scenario.id.clone(), probe.id.clone()))
                .or_default();
            counts.runs += 1;
            counts.passed += u32::from(passed);
            counts.judged += u32::from(judged);
        }
    }
    /// The probe's counted and judged runs.
    fn counts(&self, key: &(String, String)) -> Option<ProbeCounts> {
        self.probes.get(key).copied()
    }
    fn rate(&self, key: &(String, String)) -> Option<f64> {
        let counts = self.counts(key)?;
        (counts.runs > 0).then(|| counts.passed as f64 / counts.runs as f64)
    }
}

/// The §8.1 rule of a candidate arm against the current one: every probe's
/// pass rate at least `current − δ`, violations, failed runs and unjudged runs
/// no higher, and every probe has at least one judged run in each arm. There
/// is no absolute pass line.
struct Rule {
    passes: bool,
    failures: Vec<String>,
}
fn rule(current: &Metrics, candidate: &Metrics, scenarios: &[Scenario]) -> Rule {
    let mut failures = Vec::new();
    for (arm, metrics) in [("current", current), ("candidate", candidate)] {
        if metrics.judged == 0 {
            failures.push(format!("the {arm} arm has no judged run"));
        }
    }
    for scenario in scenarios {
        for probe in &scenario.probes {
            let key = (scenario.id.clone(), probe.id.clone());
            let current_counts = current.counts(&key);
            let candidate_counts = candidate.counts(&key);
            for (arm, counts) in [("current", current_counts), ("candidate", candidate_counts)] {
                if counts.is_none_or(|counts| counts.judged == 0) {
                    failures.push(format!(
                        "{}/{} has no judged run in the {arm} arm",
                        scenario.id, probe.id
                    ));
                }
            }
            let (Some(current_counts), Some(candidate_counts)) = (current_counts, candidate_counts)
            else {
                continue;
            };
            let (current_passed, current_runs) = (current_counts.passed, current_counts.runs);
            let (candidate_passed, candidate_runs) =
                (candidate_counts.passed, candidate_counts.runs);
            // §8.1 in integers (δ = 1/20): kp/kn ≥ cp/cn − 1/20
            // ⇔ 20·kp·cn + cn·kn ≥ 20·cp·kn.
            let (numerator, denominator) = DELTA;
            let candidate_side = denominator * candidate_passed as u64 * current_runs as u64
                + numerator * current_runs as u64 * candidate_runs as u64;
            let current_side = denominator * current_passed as u64 * candidate_runs as u64;
            if candidate_side < current_side {
                failures.push(format!(
                    "{}/{} falls from {:.3} to {:.3} (δ = {})",
                    scenario.id,
                    probe.id,
                    current_passed as f64 / current_runs as f64,
                    candidate_passed as f64 / candidate_runs as f64,
                    delta(),
                ));
            }
        }
    }
    if candidate.violations > current.violations {
        failures.push(format!(
            "violations rise from {} to {}",
            current.violations, candidate.violations
        ));
    }
    if candidate.failed > current.failed {
        failures.push(format!(
            "failed runs rise from {} to {}",
            current.failed, candidate.failed
        ));
    }
    if candidate.unjudged > current.unjudged {
        failures.push(format!(
            "unjudged runs rise from {} to {}",
            current.unjudged, candidate.unjudged
        ));
    }
    Rule {
        passes: failures.is_empty(),
        failures,
    }
}

/// The instrument of an evaluation: the text under test, the model and the
/// judge with the configuration they carry, the runs and the candidate text's
/// cost (model-prompts §8.1). The configurations are read from the first run,
/// which resolves them.
struct Instrument {
    text: String,
    model: ModelId,
    judge: ModelId,
    runs: usize,
    tokens: f64,
    model_configuration: Option<Configuration>,
    judge_configuration: Option<Configuration>,
    /// The judge's declared limits, read from the first run's selection.
    judge_window: Option<JudgeWindow>,
    judge_temperature_dropped: usize,
    /// Runs the judge was asked about: every outcome but `not_asked` and a
    /// view that does not fit its budget.
    judge_asked_runs: usize,
    /// How many re-asks the judge was asked for over every run.
    judge_reasks: usize,
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
    let mut text = format!(
        "| | |\n| --- | --- |\n| text | `{}` |\n| model | `{}/{}`, {} |\n| judge | `{}/{}`, temperature 0, {}{window} |\n| runs | {} per arm and scenario |\n| candidate tokens | {:.0} |\n| rule | every probe is at least the current arm's − {delta:.2}, and violations, failed runs and unjudged runs are no higher |\n",
        instrument.text,
        instrument.model.provider_id,
        instrument.model.model_id,
        carried(&instrument.model_configuration),
        instrument.judge.provider_id,
        instrument.judge.model_id,
        carried(&instrument.judge_configuration),
        instrument.runs,
        instrument.tokens,
        delta = delta(),
    );
    if same_family(&instrument.model, &instrument.judge) {
        text += "| judge limitation | the judge is the model's own family; the measurement is limited by that |\n";
    }
    if instrument.judge_temperature_dropped > 0 {
        text += &format!(
            "| judge temperature | dropped in {} of {} runs the judge was asked |\n",
            instrument.judge_temperature_dropped, instrument.judge_asked_runs
        );
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
    text +=
        "\n| scenario | probe | kind | current | candidate |\n| --- | --- | --- | --- | --- |\n";
    for scenario in scenarios {
        for probe in &scenario.probes {
            let key = (scenario.id.clone(), probe.id.clone());
            let cell = |metrics: Option<&Metrics>| match metrics.and_then(|m| m.rate(&key)) {
                Some(rate) => format!("{rate:.3}"),
                None => "–".to_owned(),
            };
            text += &format!(
                "| {} | {} | {} | {} | {} |\n",
                scenario.id,
                probe.id,
                probe.kind.name(),
                cell(metrics.get("current")),
                cell(metrics.get("candidate")),
            );
        }
    }
    text += "\n| arm | violations | failed runs | unjudged runs |\n| --- | --- | --- | --- |\n";
    for arm in ["current", "candidate"] {
        let empty = Metrics::default();
        let metrics = metrics.get(arm).unwrap_or(&empty);
        text += &format!(
            "| {arm} | {} | {} | {} |\n",
            metrics.violations, metrics.failed, metrics.unjudged
        );
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
    if let (Some(current), Some(candidate)) = (metrics.get("current"), metrics.get("candidate")) {
        let rule = rule(current, candidate, scenarios);
        text += &if rule.passes {
            format!(
                "\ncandidate passes: every probe is at least the current arm's − {delta:.2}, and violations, failed runs and unjudged runs are no higher.\n",
                delta = delta(),
            )
        } else {
            format!("\ncandidate fails: {}.\n", rule.failures.join("; "))
        };
    }
    text
}

/// One run's private endpoint directory, short enough for Unix socket addresses.
/// Dropping it removes only that run's endpoints.
struct Sockets(PathBuf);
impl Sockets {
    fn new() -> Result<Self> {
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
fn private_directory(path: &Path) -> Result<()> {
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
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
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

#[cfg(test)]
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
            temperature_dropped: false,
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
            text: "context-compaction/summary".into(),
            model: model(),
            judge,
            runs: 1,
            tokens: 1.,
            model_configuration: None,
            judge_configuration: None,
            judge_window: None,
            judge_temperature_dropped: 0,
            judge_asked_runs: 0,
            judge_reasks: 0,
        }
    }

    /// An unknown `--text` id fails before any run and lists the known ids.
    #[test]
    fn an_unknown_text_id_fails_before_any_run() {
        let error = overrides("no-such-text", Arc::from("candidate"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no-such-text"), "{error}");
        assert!(error.contains("context-compaction/summary"), "{error}");
        assert!(
            error.contains("timeline-tree/branch-description"),
            "{error}"
        );
        let known = overrides("context-compaction/summary", Arc::from("candidate")).unwrap();
        assert_eq!(known.summary_prompt.as_deref(), Some("candidate"));
        let branch = overrides("timeline-tree/branch-description", Arc::from("candidate")).unwrap();
        assert_eq!(branch.branch_description.as_deref(), Some("candidate"));
    }

    /// Goal's two texts are its manifest `config` members, patched into the
    /// plugin's row by name (Goal A.5.1, model-prompts §8.1).
    #[test]
    fn the_goal_texts_are_manifest_config_members() {
        for (id, member) in [
            ("goal/objective", "objective"),
            ("goal/continuation", "continuation"),
        ] {
            let overrides = overrides(id, Arc::from("candidate")).unwrap();
            assert_eq!(
                overrides.plugin_configs["goal"]["texts"][member],
                json!("candidate"),
                "{id}"
            );
        }
    }

    /// A candidate exactly at `current − δ` passes the rule; one just below
    /// fails it, over run counts whose rates as `f64` sit either side of the
    /// boundary (model-prompts §8.1).
    #[test]
    fn the_rule_passes_at_the_current_minus_delta_and_fails_below_it() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let arm = |arm: &'static str, passed: usize, runs: usize| {
            let records: Vec<_> = (0..runs)
                .map(|index| {
                    let verdict = if index < passed {
                        ProbeVerdict::Present
                    } else {
                        ProbeVerdict::Absent
                    };
                    record(
                        arm,
                        &scenario,
                        &[("reports", verdict)],
                        JudgeOutcome::Judged,
                    )
                })
                .collect();
            metrics(&scenario, &records)
        };
        for (current, candidate) in [((20, 20), (19, 20)), ((2, 5), (7, 20))] {
            let verdict = rule(
                &arm("current", current.0, current.1),
                &arm("candidate", candidate.0, candidate.1),
                std::slice::from_ref(&scenario),
            );
            assert!(
                verdict.passes,
                "{}/{} → {}/{} is exactly the current arm's − δ: {:?}",
                current.0, current.1, candidate.0, candidate.1, verdict.failures
            );
        }
        for (current, candidate) in [((20, 20), (18, 20)), ((2, 5), (6, 20))] {
            let verdict = rule(
                &arm("current", current.0, current.1),
                &arm("candidate", candidate.0, candidate.1),
                std::slice::from_ref(&scenario),
            );
            assert!(
                !verdict.passes,
                "{}/{} → {}/{} is below the current arm's − δ",
                current.0, current.1, candidate.0, candidate.1
            );
            assert!(
                verdict
                    .failures
                    .iter()
                    .any(|failure| failure.contains("reports")),
                "{:?}",
                verdict.failures
            );
        }
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

    /// More violations or more failed runs fail the candidate even when every
    /// probe's pass rate stays inside δ.
    #[test]
    fn more_violations_or_failed_runs_fail_the_candidate() {
        let scenario = scenario(vec![
            probe("reports", ProbeKind::MustShow),
            probe("floods", ProbeKind::MustNotShow),
        ]);
        let current = metrics(
            &scenario,
            &(0..20)
                .map(|_| {
                    record(
                        "current",
                        &scenario,
                        &[
                            ("reports", ProbeVerdict::Present),
                            ("floods", ProbeVerdict::Absent),
                        ],
                        JudgeOutcome::Judged,
                    )
                })
                .collect::<Vec<_>>(),
        );
        let one_violation: Vec<RunRecord> = (0..20)
            .map(|index| {
                let floods = if index == 0 {
                    ProbeVerdict::Present
                } else {
                    ProbeVerdict::Absent
                };
                record(
                    "candidate",
                    &scenario,
                    &[("reports", ProbeVerdict::Present), ("floods", floods)],
                    JudgeOutcome::Judged,
                )
            })
            .collect();
        let verdict = rule(
            &current,
            &metrics(&scenario, &one_violation),
            std::slice::from_ref(&scenario),
        );
        assert!(!verdict.passes);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("violations rise")),
            "{:?}",
            verdict.failures
        );

        let one_failure: Vec<RunRecord> = (0..20)
            .map(|index| {
                record(
                    "candidate",
                    &scenario,
                    &[
                        ("reports", ProbeVerdict::Present),
                        ("floods", ProbeVerdict::Absent),
                    ],
                    if index == 0 {
                        JudgeOutcome::CallFailed
                    } else {
                        JudgeOutcome::Judged
                    },
                )
            })
            .collect();
        let verdict = rule(
            &current,
            &metrics(&scenario, &one_failure),
            std::slice::from_ref(&scenario),
        );
        assert!(!verdict.passes);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("failed runs rise")),
            "{:?}",
            verdict.failures
        );
    }

    /// Unjudged runs are compared between the arms the way failed runs are: a
    /// candidate with more of them fails the rule, and equal counts do not
    /// fail it on their own (model-prompts §8.1).
    #[test]
    fn more_unjudged_runs_fail_the_candidate() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let arm = |arm: &'static str, judged: usize, unjudged: usize| {
            let records: Vec<_> = (0..judged)
                .map(|_| {
                    record(
                        arm,
                        &scenario,
                        &[("reports", ProbeVerdict::Present)],
                        JudgeOutcome::Judged,
                    )
                })
                .chain((0..unjudged).map(|_| {
                    let mut run = record(
                        arm,
                        &scenario,
                        &[("reports", ProbeVerdict::Unparsed)],
                        JudgeOutcome::Unreadable,
                    );
                    run.judge_error = Some("the judge answered with no usable verdicts".into());
                    run
                }))
                .collect();
            metrics(&scenario, &records)
        };
        let verdict = rule(
            &arm("current", 20, 0),
            &arm("candidate", 19, 1),
            std::slice::from_ref(&scenario),
        );
        assert!(!verdict.passes);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure == "unjudged runs rise from 0 to 1"),
            "{:?}",
            verdict.failures
        );
        let verdict = rule(
            &arm("current", 19, 1),
            &arm("candidate", 19, 1),
            std::slice::from_ref(&scenario),
        );
        assert!(verdict.passes, "{:?}", verdict.failures);
    }

    /// A probe with no judged run in one arm cannot be compared: the rule
    /// names the arm rather than reading the missing run as a fall (issue
    /// #642).
    #[test]
    fn a_probe_with_no_judged_run_in_an_arm_fails_the_rule() {
        let scenario = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let current = metrics(
            &scenario,
            &[record(
                "current",
                &scenario,
                &[("reports", ProbeVerdict::Present)],
                JudgeOutcome::Judged,
            )],
        );
        let mut unjudged = record(
            "candidate",
            &scenario,
            &[("reports", ProbeVerdict::Unparsed)],
            JudgeOutcome::Unreadable,
        );
        unjudged.judge_error = Some("the judge answered with no usable verdicts: no answer".into());
        let verdict = rule(
            &current,
            &metrics(&scenario, std::slice::from_ref(&unjudged)),
            std::slice::from_ref(&scenario),
        );
        assert!(!verdict.passes);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure == "case/reports has no judged run in the candidate arm"),
            "{:?}",
            verdict.failures
        );
        assert!(
            !verdict
                .failures
                .iter()
                .any(|failure| failure.contains("falls from")),
            "a missing judged run never reads as a fall: {:?}",
            verdict.failures
        );
    }

    /// A probe needs a judged run in each arm, not merely a counted one
    /// (model-prompts §8.1, issue #642): a run whose judge call failed still
    /// counts toward the probe's rate, so a probe whose every run in an arm
    /// ended that way has no judged run there, and the rule names it. One
    /// judged run in the arm satisfies the floor.
    #[test]
    fn a_probe_needs_a_judged_run_in_each_arm() {
        let case = scenario(vec![probe("reports", ProbeKind::MustShow)]);
        let mut other = scenario(vec![probe("fine", ProbeKind::MustShow)]);
        other.id = "other".into();
        let scenarios = [case, other];
        let (case, other) = (&scenarios[0], &scenarios[1]);
        // One arm: of the probe's runs, the judge answered `judged` of them
        // with a verdict that fails it and its calls failed for the rest; one
        // judged run of `other` keeps the arm itself off the arm-level
        // message.
        let arm = |arm: &'static str, judged: usize, call_failed: usize, with_other: bool| {
            let mut metrics = Metrics::default();
            for _ in 0..judged {
                metrics.add(
                    case,
                    &record(
                        arm,
                        case,
                        &[("reports", ProbeVerdict::Absent)],
                        JudgeOutcome::Judged,
                    ),
                );
            }
            for _ in 0..call_failed {
                metrics.add(
                    case,
                    &record(
                        arm,
                        case,
                        &[("reports", ProbeVerdict::Unparsed)],
                        JudgeOutcome::CallFailed,
                    ),
                );
            }
            if with_other {
                metrics.add(
                    other,
                    &record(
                        arm,
                        other,
                        &[("fine", ProbeVerdict::Present)],
                        JudgeOutcome::Judged,
                    ),
                );
            }
            metrics
        };
        // Every one of the probe's runs in the candidate arm ended in a
        // failed judge call, while the arm has a judged run elsewhere: the
        // rule names the probe in the candidate arm alone.
        let verdict = rule(
            &arm("current", 1, 2, true),
            &arm("candidate", 0, 2, true),
            &scenarios,
        );
        assert!(!verdict.passes);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure == "case/reports has no judged run in the candidate arm"),
            "{:?}",
            verdict.failures
        );
        assert!(
            !verdict
                .failures
                .iter()
                .any(|failure| failure == "case/reports has no judged run in the current arm"),
            "{:?}",
            verdict.failures
        );
        // An arm with no judged run at all still gets the arm-level message
        // beside the probe's.
        let verdict = rule(
            &arm("current", 1, 2, true),
            &arm("candidate", 0, 2, false),
            &scenarios,
        );
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure == "the candidate arm has no judged run"),
            "{:?}",
            verdict.failures
        );
        // One judged run in the arm satisfies the probe's floor.
        let verdict = rule(
            &arm("current", 1, 2, true),
            &arm("candidate", 1, 2, true),
            &scenarios,
        );
        assert!(verdict.passes, "{:?}", verdict.failures);
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
        assert!(text.contains("| candidate | 0 | 1 | 0 |"), "{text}");
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
        assert!(text.contains("| candidate | 0 | 0 | 1 |"), "{text}");
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
        assert_eq!(metrics.judged, 0);
        assert_eq!(
            metrics.counts(&("case".to_owned(), "reports".to_owned())),
            None,
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
                    ..Default::default()
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
        assert!(report.contains("candidate passes"), "{report}");
        assert_eq!(fixture.judge().len(), 2, "one judge request per arm");
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["judgeReasks"], 0, "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
            assert_eq!(record["probes"][0]["verdict"], "present", "{record}");
            assert_eq!(record["probes"][1]["verdict"], "absent", "{record}");
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
            report.contains("| case | floods | must_not_show | 1.000 | 1.000 |"),
            "a contradicted verdict passes the probe: {report}"
        );
        assert!(report.contains("| current | 0 | 0 | 0 |"), "{report}");
        assert!(report.contains("| candidate | 0 | 0 | 0 |"), "{report}");
        assert!(report.contains("candidate passes"), "{report}");
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
        assert!(report.contains("candidate passes"), "{report}");
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["judgeReasks"], 1, "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
            assert_eq!(record["probes"][0]["verdict"], "present", "{record}");
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
        assert!(report.contains("candidate passes"), "{report}");
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
        assert!(report.contains("| current | 0 | 1 | 0 |"), "{report}");
        assert!(report.contains("| candidate | 0 | 1 | 0 |"), "{report}");
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
        assert!(report.contains("| current | 0 | 1 | 0 |"), "{report}");
        assert!(report.contains("| candidate | 0 | 1 | 0 |"), "{report}");
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

    /// The provider's answer to one request: the stream event to send, or
    /// `None` for a plain text reply.
    type Reply = fn(&str) -> Option<Value>;
    /// How long the provider holds a request's whole answer back before it
    /// sends the first byte of it; `None` sends the answer at once.
    type Hold = fn(&str) -> Option<Duration>;
    /// The hold of a provider that answers every request at once.
    fn holds_nothing(_: &str) -> Option<Duration> {
        None
    }

    /// One loopback OpenAI-compatible provider that answers by `reply`, holds
    /// an answer by `hold`, and records every request it receives.
    struct Provider {
        requests: Mutex<Vec<String>>,
        reply: Reply,
        hold: Hold,
    }
    async fn completion(
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
    fn content(text: &str) -> Value {
        json!({"id":"fixture","choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
    }
    /// A tool call in the stream shape the provider speaks.
    fn tool_call(name: &str, arguments: &str) -> Value {
        json!({"id":"fixture","choices":[{"index":0,"delta":{"tool_calls":[{
            "index":0,"id":"call-1","type":"function",
            "function":{"name":name,"arguments":arguments}
        }]},"finish_reason":"tool_calls"}]})
    }
    /// The judge's answer to a judge request, and `None` for any other.
    fn judged(text: &str) -> Option<Value> {
        text.contains("You check statements")
            .then(|| content(r#"[{"id":"ran","verdict":"present","quote":"turn/end"}]"#))
    }
    /// How many times one turn reads the workspace file before it answers.
    const READS: usize = 4;
    /// The fixture model's declared window in the bytes of a request body, at
    /// the product's four bytes per token.
    const WINDOW_BYTES: usize = 24_000 * 4;
    /// The judge's answers, its refusal of a request its window cannot hold,
    /// the way a judge model refuses one over its window, and a `read` of the
    /// workspace file for a run request that has not read it [`READS`] times.
    fn reads_until_the_window_overflows(text: &str) -> Option<Value> {
        if text.contains("You check statements") {
            return (text.len() > WINDOW_BYTES)
                .then(|| refusal("This model's maximum context length is 24000 tokens"))
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
    /// The judge's verdict, and a `read` of the workspace's file for a run
    /// request that does not carry the file's text yet.
    fn reads_the_workspace_file(text: &str) -> Option<Value> {
        judged(text).or_else(|| {
            (!text.contains(FILE_TEXT)).then(|| tool_call("read", r#"{"path":"notes.txt"}"#))
        })
    }

    /// One disposable product home, one scenario and one loopback provider:
    /// the fixture of the end-to-end runs. It stops the provider and removes
    /// its directory when it drops.
    struct Fixture {
        directory: PathBuf,
        provider: Arc<Provider>,
        http: tokio::task::JoinHandle<()>,
        home: PathBuf,
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
            Self::holding(label, reply, holds_nothing, scenario, workspace).await
        }
        /// [`start`](Self::start) with a provider that holds the answers
        /// [`Hold`] names before it sends them.
        async fn holding(
            label: &str,
            reply: Reply,
            hold: Hold,
            scenario: Value,
            workspace: &[(&str, &str)],
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
            let directory = std::env::temp_dir()
                .join(format!("dim-scenario-eval-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            let home = directory.join("home");
            std::fs::create_dir_all(&home).unwrap();
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
                                "contextWindow": 24000,
                                "maxOutputTokens": 200,
                                "config": {"maxTokens": 200}
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
                                "maxOutputTokens": 200,
                                "config": {"maxTokens": 200}
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
            let mut platform = HostPlatform::current(shell).unwrap();
            platform.home = platform_home;
            Self {
                directory,
                provider,
                http,
                home,
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
            evaluate(options, |builder| {
                builder.process_value(Arc::new(self.platform.clone()));
            })
            .await
        }
        /// The options of one run per arm over this fixture, with `out`.
        fn options(&self, out: PathBuf) -> Options {
            Options {
                scenarios: self.scenarios.clone(),
                text: "context-compaction/summary".into(),
                candidate: Arc::from(CANDIDATE),
                model: self.target(),
                judge: self.target(),
                runs: 1,
                out,
                home: self.home.clone(),
                model_config: Some(self.models.clone()),
                defaults: None,
                node: None,
                js_runtime: None,
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
        let mut instrument = Instrument {
            text: options.text.clone(),
            model: options.model.clone(),
            judge: options.judge.clone(),
            runs: options.runs,
            tokens: 0.,
            model_configuration: None,
            judge_configuration: None,
            judge_window: None,
            judge_temperature_dropped: 0,
            judge_asked_runs: 0,
            judge_reasks: 0,
        };
        run_once(
            options,
            &fixture.home,
            arm,
            overrides,
            &|builder| {
                builder.process_value(Arc::new(fixture.platform.clone()));
            },
            &scenario,
            run,
            &CancellationToken::new(),
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

    /// A message that crosses the fixture's compaction threshold: the system
    /// prompt and the product's tool catalog take about 7,200 tokens of the
    /// 24,000-token window, compaction triggers at 80% of it (19,040), and
    /// 56,000 bytes count as 14,000 tokens at the product's 4 bytes per
    /// token, so the request fits the window and still crosses the trigger.
    fn compaction_message() -> String {
        format!("first {}", "x".repeat(56_000))
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
        let fixture = Fixture::start(
            "candidate",
            summaries_and_judgements,
            json!({
                "message": compaction_message(),
                "steers": [{"afterSeconds": 1.0, "text": "second"}],
                "budget": {"turns": 6, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.judge_options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate passes"), "{report}");
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert_eq!(record["turns"], 2, "the steer's turn ran: {record}");
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
        // The compaction changes this run's history: the request right after
        // the change carries the whole message list, not the messages added
        // since.
        for arm in ["current", "candidate"] {
            let directory = options.out.join(arm).join("case").join("run-0");
            assert_view_rebuilds(&directory);
            let export = std::fs::read_to_string(directory.join("timeline.jsonl")).unwrap();
            let view = std::fs::read_to_string(directory.join("judge-view.jsonl")).unwrap();
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
        const WINDOW: u64 = 24_000;
        let fixture = Fixture::start(
            "judge-view",
            reads_until_the_window_overflows,
            json!({
                "message": "Read notes.txt until you are sure of it.",
                "budget": {"turns": 4, "seconds": 60, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[("notes.txt", FILE_TEXT)],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate passes"), "{report}");
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
            window_facts(WINDOW, 200, &request).count_tokens(&request)
        };
        for arm in ["current", "candidate"] {
            let directory = options.out.join(arm).join("case").join("run-0");
            let export = std::fs::read_to_string(directory.join("timeline.jsonl")).unwrap();
            let view = std::fs::read_to_string(directory.join("judge-view.jsonl")).unwrap();
            assert!(
                estimate(&export) > WINDOW as f64,
                "{arm}: the raw export is longer than the judge's window"
            );
            assert!(
                estimate(&view) < WINDOW as f64,
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
        let export = std::fs::read_to_string(directory.join("timeline.jsonl")).unwrap();
        let view = std::fs::read_to_string(directory.join("judge-view.jsonl")).unwrap();
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
        assert!(report.contains("| current | 0 | 0 | 1 |"), "{report}");
        assert!(report.contains("| candidate | 0 | 0 | 1 |"), "{report}");
        assert!(
            report.contains("| current | over | 0 | the judge view needs about "),
            "the report lists the unjudged run with its estimate and budget: {report}"
        );
        assert!(
            report.contains("| candidate | over | 0 | the judge view needs about "),
            "{report}"
        );
        assert!(
            report.contains("| case | ran | must_show | 1.000 | 1.000 |"),
            "the runs whose view fits are judged: {report}"
        );
        assert!(
            report.contains("| over | ran | must_show | – | – |"),
            "an unjudged run is out of the probe rates: {report}"
        );
        assert!(
            report.contains(
                "candidate fails: over/ran has no judged run in the current arm; over/ran has no judged run in the candidate arm"
            ),
            "the judged-run floor names the runs the judge was not asked about: {report}"
        );
    }

    /// A run offers the shipped assembly's whole tool catalog — the workspace
    /// tools and Questions, not only the mock profile's three — carries a
    /// workspace tool's result into its next request (model-prompts §8.1), and
    /// fires a steer on that tool's call.
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
        assert!(report.contains("candidate passes"), "{report}");
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert_eq!(record["turns"], 2, "the steer's turn ran: {record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
        }
        for arm in ["current", "candidate"] {
            let requests = fixture.arm(arm);
            assert_eq!(
                requests.len(),
                3,
                "{arm}: the tool call, the answer after it, and the steer's turn"
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
            for tool in ["read", "write", "edit", "glob", "grep", "exec", "ask_user"] {
                assert!(
                    offered.contains(&tool),
                    "{arm}: {tool} is offered: {offered:?}"
                );
            }
            assert!(
                requests[1].contains(FILE_TEXT),
                "{arm}: the read result reaches the next request"
            );
            assert!(
                requests[2].contains(STEER),
                "{arm}: the steer's text reaches the next turn"
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
                "--home",
                fixture.home.to_str().unwrap(),
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
        assert!(report.contains("candidate passes"), "{report}");
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
        }
        assert!(options.out.join("current/case/run-0/workspace").is_dir());
    }

    /// A question is declined rather than awaited: the run's next request
    /// carries the refusal, and the run ends its turn instead of sitting on
    /// its wall-clock budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_question_is_declined_and_the_run_ends() {
        /// The judge's verdict, and an `ask_user` call for a run request that
        /// does not carry the refusal yet.
        fn asks_a_question(text: &str) -> Option<Value> {
            judged(text).or_else(|| {
                (!text.contains("User declined to answer")).then(|| {
                    tool_call(
                        "ask_user",
                        r#"{"questions":[{"question":"Where to deploy?","options":[{"label":"production"},{"label":"staging"}]}]}"#,
                    )
                })
            })
        }
        let fixture = Fixture::start(
            "question",
            asks_a_question,
            json!({
                "message": "Ask me where to deploy.",
                "budget": {"turns": 4, "seconds": 120, "window": 24000},
                "probes": [{"id": "ran", "kind": "must_show", "text": "the run has a transcript"}]
            }),
            &[],
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(report.contains("candidate passes"), "{report}");
        let records = fixture.records(&options.out);
        assert_eq!(records.len(), 2, "one run per arm");
        for record in &records {
            assert_eq!(record["stop"], "idle", "{record}");
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
        }
        for arm in ["current", "candidate"] {
            let requests = fixture.arm(arm);
            assert_eq!(requests.len(), 2, "{arm}: the question and the answer");
            assert!(
                requests[1].contains("User declined to answer. Proceed without these answers."),
                "{arm}: the model reads the refusal"
            );
        }
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
            report.contains("candidate passes"),
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
        )
        .await;
        let options = fixture.options(fixture.directory.join("out"));
        let report = fixture.evaluate(&options).await.unwrap();
        assert!(
            report.contains("candidate passes"),
            "{report}\nrecords: {:?}",
            fixture.records(&options.out)
        );
        for arm in ["current", "candidate"] {
            let export = std::fs::read_to_string(
                options
                    .out
                    .join(arm)
                    .join("case")
                    .join("run-0")
                    .join("timeline.jsonl"),
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
                    .join(arm)
                    .join("case")
                    .join("run-0")
                    .join("timeline.jsonl"),
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

    /// A candidate for a TypeScript plugin's text replaces that member of
    /// the plugin's factory configuration: the candidate arm's request
    /// carries it, and the current arm carries the shipped text (issue #612,
    /// acceptance 3).
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
            overrides_in(FIXTURE_TEXTS, "fixture/prompt", Arc::from(CANDIDATE)).unwrap();
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
    }
}

//! `scenario-eval`: the scenario runner for policy texts (model-prompts §8.1,
//! §8.2 and §9.8; Timeline branches §12.3). It calls real models, so it is not
//! part of CI: the operator runs it against an isolated product home or a
//! model configuration, and the output goes outside the repository.
//!
//! ```text
//! cargo run --locked -p xtask -- scenario-eval --scenarios DIR --text ID
//!     --candidate FILE [--text ID --candidate FILE]... --model PROVIDER/MODEL
//!     --judge PROVIDER/MODEL --runs N --home DIR --out DIR [--jobs N]
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
//! runs at once; each run keeps its own workspace copy, product home, local
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
//! runtime/plugins/<name> --ignore-scripts` for `goal`,
//! `workflow` and `scheduler`, the default plugins that ship a
//! `package.json` (`plugin-authoring` ships only a Skill).
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
//! and on error, without affecting other runs. A run's home, Session, judge
//! calls and endpoint are its own, so `--jobs` needs nothing of them: every
//! run serves its own App Server on its own endpoint, and the runner holds no
//! state a second run reads.
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
    context_model::window_facts,
    host::{
        ModelHost, ModelSelectionHost, ModelStopReason, ProviderConnectionResolver,
        ResolveContextModel,
    },
};
use dim_agent_runtime::AgentBuilder;
use dim_agent_types::{
    InputMode, Message, ModelConfig, ModelId, ModelRequest, ModelSelection, Role, SystemPrompt,
};
use dim_app_server::RunningServer;
use dim_client::{Client, ConnectionConfig, Message as WireMessage, Server as WireServer};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    sync::Arc,
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
    let report = match (&options.rejudge, options.pool.is_empty()) {
        (Some(source), _) => rejudge(&options, source, |_| {}).await?,
        (None, false) => pooled(&options, |_| {}).await?,
        (None, true) => evaluate(&options, |_| {}).await?,
    };
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
    let candidate = overrides(&options.texts)?;
    let current = Overrides::default();
    let scenarios = read_scenarios(&options.scenarios)?;
    let home = isolated_home(&options.home)?;
    if let Some(path) = &options.model_config {
        dim_app_server::cli::model_profile(path)
            .map_err(|error| format!("--model-config {}: {error}", path.display()))?;
    }
    let cancel = CancellationToken::new();
    let instrument = Instrument::new(options);
    // Every run keeps its own instrument, merged in schedule order below, so
    // nothing in the report depends on which run finished first.
    let schedule = schedule(options.runs, scenarios.len());
    let mut pending: Vec<Option<(RunRecord, Instrument)>> =
        (0..schedule.len()).map(|_| None).collect();
    let (home, observe, scenarios, cancel) = (&home, &observe, &scenarios, &cancel);
    let planned = schedule
        .iter()
        .enumerate()
        .map(|(index, &(arm, scenario, run))| {
            let mut measured = instrument.clone();
            let overrides = match arm {
                "current" => &current,
                _ => &candidate,
            };
            async move {
                let record = run_once(
                    options,
                    home,
                    arm,
                    overrides,
                    observe,
                    &scenarios[scenario],
                    run,
                    cancel,
                    &mut measured,
                )
                .await?;
                Ok::<_, Box<dyn std::error::Error>>((index, record, measured))
            }
        });
    let mut in_flight = futures_util::stream::iter(planned).buffer_unordered(options.jobs);
    while let Some(finished) = in_flight.next().await {
        let (index, record, measured) = finished?;
        pending[index] = Some((record, measured));
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
        let directory = source
            .join(&saved.arm)
            .join(&saved.scenario)
            .join(format!("run-{}", saved.run));
        let path = directory.join("judge-view.jsonl");
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
    #[serde(default)]
    probes: Vec<SavedProbe>,
}

/// One probe's record in a saved run: the kind the run was judged against,
/// its verdict and the judge's quote.
#[derive(serde::Deserialize)]
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
    home: PathBuf,
    model_config: Option<PathBuf>,
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
                     --home DIR --out DIR [--jobs N] [--model-config FILE] \
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
        let (mut scenarios, mut home, mut out) = (None, None, None);
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
                "--home" => home = Some(PathBuf::from(value)),
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
        if finished && (!texts.is_empty() || runs.is_some() || home.is_some() || defaults.is_some())
        {
            return Err(
                "--rejudge and --pool read finished evaluations' saved runs: --text, \
                 --candidate, --runs, --home and --defaults belong to a new evaluation"
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
        let home = if finished {
            PathBuf::new()
        } else {
            home.ok_or(usage)?
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
            home,
            model_config,
            defaults,
            node: env_path("DIM_TEST_NODE"),
            js_runtime: env_path("DIM_TEST_JS_RUNTIME"),
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
#[derive(Clone, serde::Serialize)]
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
        judge_calls: finished.judging.calls,
        temperature_dropped: Some(finished.judging.temperature_dropped),
        decline_retries: Some(driven.decline_retries),
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
    /// How many declines the runner sent again after `question_not_pending`.
    decline_retries: u32,
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
    // An input the runner submitted is outstanding until a turn starts for it
    // or the run consumes it; a line that has done neither is not idle.
    let mut awaiting = true;
    // The declines whose `ask_user` call the Session had not registered when
    // they were sent, kept until the call's `step/tool-result` closes it.
    let mut declines: Vec<Decline> = Vec::new();
    loop {
        match fire_due(client, &session, &mut steers, started).await {
            Ok(submitted) => awaiting |= submitted,
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

/// A question the runner declined that the Session has no waiter for yet. The
/// Questions capability registers a call's waiter inside the tool handler
/// (Questions §3.1), so a decline that arrives before the handler ran is
/// `question_not_pending`: the client keeps its prompt until the call's
/// `step/tool-result` arrives and lets the user answer again (§3.2, A12).
struct Decline {
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
async fn decline_question(
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
async fn retry_declines(
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
    let mut calls: Vec<JudgeCall> = Vec::new();
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
            judge_calls: Vec::new(),
            temperature_dropped: Some(false),
            decline_retries: Some(0),
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

    /// Goal's two texts are its manifest `config` members, patched into the
    /// plugin's row by name; two pairs of one plugin are two members of one
    /// configuration, not two configurations (Goal A.5.1, model-prompts §8.1).
    #[test]
    fn the_goal_texts_are_manifest_config_members() {
        for (id, member) in [
            ("goal/objective", "objective"),
            ("goal/continuation", "continuation"),
        ] {
            let overrides = overrides(&pairs(&[(id, "candidate")])).unwrap();
            assert_eq!(
                overrides.plugin_configs["goal"]["texts"][member],
                json!("candidate"),
                "{id}"
            );
        }
        let both = overrides(&pairs(&[
            ("goal/objective", "first"),
            ("goal/continuation", "second"),
        ]))
        .unwrap();
        assert_eq!(
            both.plugin_configs["goal"]["texts"]["objective"],
            json!("first")
        );
        assert_eq!(
            both.plugin_configs["goal"]["texts"]["continuation"],
            json!("second")
        );
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
            "--home",
            "home",
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
                "--home",
                "home",
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
                    "--home",
                    "home",
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
            let directory = source.join(arm).join("case").join(format!("run-{run}"));
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("judge-view.jsonl"), view).unwrap();
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
    const READS: usize = 3;
    /// The fixture models' output allowance: their `maxOutputTokens` and the
    /// requests' `maxTokens`. The input budget is the window less this.
    const OUTPUT_TOKENS: u64 = 200;
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
        /// [`rejudge`] over this fixture's provider, with the same process
        /// facts as [`evaluate`](Self::evaluate).
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
                home: self.home.clone(),
                model_config: Some(self.models.clone()),
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
                .join("current")
                .join("case")
                .join("run-0")
                .join("timeline.jsonl"),
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
            let directory = options.out.join(arm).join("case").join("run-0");
            assert_view_rebuilds(&directory);
            let export = std::fs::read_to_string(directory.join("timeline.jsonl")).unwrap();
            assert_eq!(
                submitted_mode(&export, "second"),
                Some(json!("steer")),
                "{arm}: the timed steer's input/message carries mode steer"
            );
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
            assert_eq!(record["judgeOutcome"], "judged", "{record}");
        }
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
        for arm in ["current", "candidate"] {
            let requests = fixture.arm(arm);
            assert_eq!(requests.len(), 2, "{arm}: the question and the answer");
            assert!(
                requests[1].contains("User declined to answer. Proceed without these answers."),
                "{arm}: the model reads the refusal"
            );
        }
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
    }

    /// What a scripted peer read: the `requestId` of every decline, in order,
    /// and how many it had read when it first wrote a call's
    /// `step/tool-result`.
    #[cfg(unix)]
    #[derive(Default)]
    struct PeerReads {
        declines: Mutex<Vec<String>>,
        settled_at: AtomicUsize,
    }

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
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let method = request["method"].as_str().unwrap_or_default();
                if method == "session.idle" && !settled {
                    continue;
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
                    _ => json!({"result": {}}),
                };
                write.write_all(&frame(&request, body)).await.unwrap();
                if method == "session.submit" {
                    write
                        .write_all(&notice(
                            json!({"type": "turn", "name": "start", "data": {}}),
                        ))
                        .await
                        .unwrap();
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
        let driven = drive(&mut client, &scenario, &std::env::temp_dir(), &peer_model()).await;
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
        let driven = drive(&mut client, &scenario, &std::env::temp_dir(), &peer_model()).await;
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
        let driven = drive(&mut client, &scenario, &std::env::temp_dir(), &peer_model()).await;
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
        let driven = drive(&mut client, &scenario, &std::env::temp_dir(), &peer_model()).await;
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
        let driven = drive(&mut client, &scenario, &std::env::temp_dir(), &peer_model()).await;
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
        let driven = drive(&mut client, &scenario, &std::env::temp_dir(), &peer_model()).await;
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
        let driven = drive(&mut client, &scenario, &std::env::temp_dir(), &peer_model()).await;
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
    }
}

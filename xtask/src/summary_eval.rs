//! `summary-eval`: the summary-quality evaluation of
//! [#170](https://github.com/arcships/dim/issues/170) (ContextCompaction
//! §13.4). It calls real models, so it is not part of CI: the operator runs it
//! against an isolated product home whose providers are configured, and the
//! output goes outside the repository.
use crate::app_server;
use crate::scenario_eval::text_tokens;
use dim_agent_capabilities::{
    context_block,
    context_model::ProductContextModel,
    history::{HistoryEntry, summary_message},
    host::{
        CompactionCheckpoint, CompactionCut, CompactionInput, CompactionStrategy, ContextModel,
        HostError, ModelChunk, ModelHost, ModelStream, ProviderConnectionResolver,
        ResolveContextModel, StrategyOutcome,
    },
    model_provider::invoke_with_recovery,
    summary_strategy::{
        LengthSentence, ProductSummaryStrategyConfig, SummaryReasoning, SummaryStrategy, TargetRule,
    },
};
use dim_agent_types::{
    BaselineSource, CompactionTrigger, Message, MessageContent, MessagePart, ModelConfig, ModelId,
    ModelRequest, ModelSelection, ReplacementParts, Role,
};
use dim_app_server::provider::ProviderService;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The case files: one JSON history per file, under the xtask manifest.
const CASES: &str = "summary-eval/cases";

/// The target rules evaluated once a reasoning value is chosen: no
/// instruction, and the four `(α, β)` candidates of §13.3.
const TARGETS: [(f64, f64); 4] = [(0.25, 0.05), (0.25, 0.10), (0.5, 0.05), (0.5, 0.10)];

/// A narrowed window also bounds the output allowance, so the case keeps a
/// positive input budget on a model whose own allowance is large (§13.4).
const NARROWED_OUTPUT_SHARE: f64 = 0.25;

const JUDGE_SYSTEM: &str = "You check the statements below against a summary that was written for a coding session. For each statement answer \"present\" when the summary states it, \"absent\" when the summary does not mention it, and \"contradicted\" when the summary states the opposite. Add the summary text that supports the verdict as a short quote. Answer with a JSON array only, one object per statement: {\"id\": \"<statement id>\", \"verdict\": \"present|absent|contradicted\", \"quote\": \"<summary text>\"}.";

pub async fn run() -> Result<()> {
    let options = Options::parse()?;
    let home = isolated_home(&options.home)?;
    let cases = read_cases(&cases_directory())?;
    std::fs::create_dir_all(&options.out)?;
    let cancel = CancellationToken::new();
    let server = app_server::serve_home(
        &home,
        &home,
        options.out.join("command-output"),
        app_server::RunSettings::default(),
        |_| {},
    )
    .await?;
    let providers = server
        .service::<ProviderService>()
        .ok_or("the product profile serves no provider catalog")?;
    let host = server
        .service::<ModelHost>()
        .ok_or("the product profile serves no model host")?;
    let resolver = server
        .service::<ProviderConnectionResolver>()
        .ok_or("the product profile serves no connection resolver")?;
    let model = selection_with_preferences(&providers, &options.model, &cancel).await?;
    let judge = selection_with_preferences(&providers, &options.judge, &cancel).await?;
    let summarizer = Configuration::read(model.config.as_ref());
    let judge_configuration = Configuration::read(judge.config.as_ref());
    let mut prepared = Vec::new();
    for case in &cases {
        prepared.push(Prepared::new(case, &model, &cancel).await?);
    }
    let mut results = Vec::new();
    let mut no_temperature = 0;
    for arm in arms(&options) {
        let strategy = SummaryStrategy::new(
            host.clone(),
            resolver.clone(),
            ProductSummaryStrategyConfig {
                reasoning: arm.reasoning,
                target: arm.target,
                system_prompt: arm.prompt.clone(),
                length_sentence: arm.length_sentence.clone(),
            },
        );
        let mut metrics = Metrics::default();
        for case in &prepared {
            for run in 0..options.runs {
                let outcome = run_case(&strategy, case, &cancel).await;
                let mut retried = false;
                let judged = judge_run(
                    &host,
                    &resolver,
                    &judge,
                    case,
                    &outcome,
                    &cancel,
                    &mut retried,
                )
                .await;
                no_temperature += usize::from(retried);
                let probes = probe_records(
                    &case.probes,
                    outcome.summary().as_deref().unwrap_or_default(),
                    &judged,
                );
                let configuration = outcome.configuration(&summarizer);
                metrics.add(&arm, case, &outcome, &probes, &configuration);
                let directory = options.out.join(&arm.id);
                std::fs::create_dir_all(&directory)?;
                std::fs::write(
                    directory.join(format!("{}-run{run}.json", case.id)),
                    serde_json::to_string_pretty(&json!({
                        "arm": arm.id,
                        "case": case.id,
                        "description": case.description,
                        "run": run,
                        "contextWindow": case.window,
                        "outputTokens": case.output,
                        "outcome": outcome.label(),
                        "error": outcome.error(),
                        "durationMs": outcome.duration.as_secs_f64() * 1000.,
                        "summary": outcome.summary(),
                        "diagnostics": outcome.diagnostics(),
                        "steps": outcome.steps_json(),
                        "configuration": configuration.json(),
                        "judge": {
                            "configuration": judge_configuration.json(),
                            "temperatureFallback": retried,
                        },
                        "judgeError": judged.error(),
                        "probes": probes,
                    }))? + "\n",
                )?;
            }
        }
        println!("{}", metrics.line(&arm));
        results.push((arm, metrics));
    }
    let rows: Vec<Value> = results
        .iter()
        .map(|(arm, metrics)| metrics.row(arm))
        .collect();
    let scored: Vec<(String, String)> = prepared
        .iter()
        .flat_map(|case| {
            case.probes
                .iter()
                .map(|probe| (case.id.clone(), probe.id.clone()))
        })
        .collect();
    let verdict = match results.as_slice() {
        [(_, current), (_, candidate)] if options.candidate.is_some() => {
            Some(verdict(current, candidate, &scored))
        }
        _ => None,
    };
    let mut report = table(&rows);
    report.push_str(&candidate_cost_line(
        &options.model,
        options.candidate.as_deref(),
        options.candidate_length.as_ref(),
    ));
    report.push_str(&judge_line(
        &options.judge,
        &judge_configuration,
        no_temperature,
        results.len() * prepared.len() * options.runs,
    ));
    if let Some(verdict) = &verdict {
        report.push_str(&format!("\n{verdict}\n"));
    }
    std::fs::write(options.out.join("summary.md"), report)?;
    server.shutdown();
    server.join().await?;
    if let Some(verdict) = &verdict {
        println!("{verdict}");
    }
    println!("wrote {}", options.out.join("summary.md").display());
    Ok(())
}

/// The model's conversation selection with its saved preferences, built the
/// way the bundled client builds one: it reads them from
/// `provider.management` and passes them at `session.create`
/// (ContextCompaction §13.4), so `inherit` inherits what the user chose. A
/// model with no saved preferences keeps the model config alone, and a
/// provider the catalog does not hold is an error rather than a default
/// selection.
async fn selection_with_preferences(
    providers: &Arc<ProviderService>,
    target: &ModelId,
    cancel: &CancellationToken,
) -> Result<ModelSelection> {
    let provider = providers
        .management(Some(target.provider_id.clone()), cancel)
        .await
        .map_err(|error| format!("{}: {error}", target.provider_id))?
        .providers
        .into_iter()
        .find(|provider| provider.provider_id == target.provider_id)
        .ok_or_else(|| format!("{}: no such provider in the catalog", target.provider_id))?;
    let preferences = provider
        .preferences
        .get(&target.model_id)
        .cloned()
        .unwrap_or_default();
    Ok(providers
        .configuration(Some(target.clone()), None, preferences, cancel)
        .await
        .map_err(|error| format!("{}: {error}", target.provider_id))?)
}

/// `--home` must be an isolated product home holding the providers'
/// configuration; it must never be the product home.
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

struct Options {
    home: PathBuf,
    model: ModelId,
    judge: ModelId,
    runs: usize,
    out: PathBuf,
    reasoning: Option<SummaryReasoning>,
    candidate: Option<Arc<str>>,
    candidate_length: Option<LengthSentence>,
    target: Option<TargetRule>,
}
impl Options {
    fn parse() -> Result<Self> {
        let usage = "usage: cargo run -p xtask -- summary-eval --home DIR \
                     --model PROVIDER/MODEL --judge PROVIDER/MODEL --runs N --out DIR \
                     [--reasoning inherit|off|low] \
                     [--candidate FILE [--candidate-length FILE] \
                     [--target OUTPUT_SHARE,INPUT_SHARE]]";
        let mut args = std::env::args().skip(2);
        let (mut home, mut model, mut judge, mut runs, mut out, mut reasoning) =
            (None, None, None, None, None, None);
        let (mut candidate, mut candidate_length, mut target_rule) = (None, None, None);
        while let Some(flag) = args.next() {
            let value = args.next().ok_or(format!("{flag} requires a value"))?;
            match flag.as_str() {
                "--home" => home = Some(PathBuf::from(value)),
                "--model" => model = Some(target(&value)?),
                "--judge" => judge = Some(target(&value)?),
                "--runs" => runs = Some(value.parse::<usize>()?),
                "--out" => out = Some(PathBuf::from(value)),
                "--reasoning" => reasoning = Some(reasoning_value(&value)?),
                "--candidate" => candidate = Some(candidate_prompt(Path::new(&value))?),
                "--candidate-length" => {
                    candidate_length = Some(candidate_length_sentence(Path::new(&value))?)
                }
                "--target" => target_rule = Some(target_share(&value)?),
                _ => return Err(usage.into()),
            }
        }
        let options = Self {
            home: home.ok_or(usage)?,
            model: model.ok_or(usage)?,
            judge: judge.ok_or(usage)?,
            runs: runs.filter(|runs| *runs > 0).ok_or(usage)?,
            out: out.ok_or(usage)?,
            reasoning,
            candidate,
            candidate_length,
            target: target_rule,
        };
        if options.candidate.is_some() && options.reasoning.is_none() {
            return Err("--candidate requires --reasoning".into());
        }
        if options.target.is_some() && options.candidate.is_none() {
            return Err("--target requires --candidate".into());
        }
        if options.candidate_length.is_some() && options.candidate.is_none() {
            return Err("--candidate-length requires --candidate".into());
        }
        if options.candidate_length.is_some() && options.target.is_none() {
            return Err("--candidate-length requires --target".into());
        }
        Ok(options)
    }
}

/// The candidate's system prompt, read before any model call.
fn candidate_prompt(path: &Path) -> Result<Arc<str>> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("--candidate {}: {error}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("--candidate {}: not UTF-8 text", path.display()))?;
    Ok(Arc::from(text))
}

/// The candidate's length sentence, read and checked for its one `{target}`
/// placeholder before any model call (ContextCompaction §13.3, §13.5).
fn candidate_length_sentence(path: &Path) -> Result<LengthSentence> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("--candidate-length {}: {error}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("--candidate-length {}: not UTF-8 text", path.display()))?;
    LengthSentence::new(text.as_str())
        .map_err(|error| format!("--candidate-length {}: {error}", path.display()).into())
}

/// One target rule for both candidate arms: two shares greater than 0 and at
/// most 1 (ContextCompaction §13.4).
fn target_share(value: &str) -> Result<TargetRule> {
    let (output, input) = value
        .split_once(',')
        .ok_or_else(|| format!("--target takes OUTPUT_SHARE,INPUT_SHARE, not {value}"))?;
    let share = |name: &str, value: &str| -> Result<f64> {
        let share: f64 = value
            .trim()
            .parse()
            .map_err(|_| format!("--target {name} share is not a number: {value}"))?;
        if !(share > 0. && share <= 1.) {
            return Err(format!(
                "--target {name} share must be greater than 0 and at most 1: {value}"
            )
            .into());
        }
        Ok(share)
    };
    Ok(TargetRule {
        output_share: share("output", output)?,
        input_share: share("input", input)?,
    })
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

fn reasoning_value(value: &str) -> Result<SummaryReasoning> {
    match value {
        "inherit" => Ok(SummaryReasoning::Inherit),
        "off" => Ok(SummaryReasoning::Off),
        "low" => Ok(SummaryReasoning::Low),
        _ => Err(format!("--reasoning takes inherit, off or low, not {value}").into()),
    }
}

/// One arm of the protocol: a reasoning value with either no target rule
/// (phase 1) or one of the target rules (phase 2); a candidate arm also
/// carries its prompt and, when one is given, its length sentence
/// (ContextCompaction §13.4).
struct Arm {
    id: String,
    reasoning: SummaryReasoning,
    target: Option<TargetRule>,
    prompt: Option<Arc<str>>,
    length_sentence: Option<LengthSentence>,
}
fn arms(options: &Options) -> Vec<Arm> {
    let Some(reasoning) = options.reasoning else {
        return [
            SummaryReasoning::Inherit,
            SummaryReasoning::Off,
            SummaryReasoning::Low,
        ]
        .into_iter()
        .map(|reasoning| Arm {
            id: format!("reasoning-{}", reasoning.name()),
            reasoning,
            target: None,
            prompt: None,
            length_sentence: None,
        })
        .collect();
    };
    if let Some(prompt) = &options.candidate {
        return vec![
            Arm {
                id: "current".into(),
                reasoning,
                target: options.target,
                prompt: None,
                length_sentence: None,
            },
            Arm {
                id: "candidate".into(),
                reasoning,
                target: options.target,
                prompt: Some(prompt.clone()),
                length_sentence: options.candidate_length.clone(),
            },
        ];
    }
    std::iter::once(Arm {
        id: "target-none".into(),
        reasoning,
        target: None,
        prompt: None,
        length_sentence: None,
    })
    .chain(TARGETS.into_iter().map(|(output_share, input_share)| Arm {
        id: format!("target-{}-{}", percent(output_share), percent(input_share)),
        reasoning,
        target: Some(TargetRule {
            output_share,
            input_share,
        }),
        prompt: None,
        length_sentence: None,
    }))
    .collect()
}
fn percent(share: f64) -> String {
    format!("{}", (share * 100.).round() as u64)
}

/// One compaction of a case: the whole history after its checkpoint, and the
/// cut that replaces its earlier part.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    messages: Vec<Message>,
    retain_from: usize,
}

/// A case file's two shapes: one compaction, or a chain of five
/// (ContextCompaction §13.4).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CaseFile {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    earlier_summary: Option<String>,
    #[serde(default)]
    messages: Option<Vec<Message>>,
    #[serde(default)]
    retain_from: Option<usize>,
    #[serde(default)]
    steps: Option<Vec<Step>>,
    probes: Vec<Probe>,
}

/// A case as the runner runs it, normalized to its steps.
struct Case {
    id: String,
    description: String,
    context_window: Option<u64>,
    earlier_summary: Option<String>,
    probes: Vec<Probe>,
    steps: Vec<Step>,
}
const CHAIN_STEPS: usize = 5;
impl CaseFile {
    fn into_case(self) -> std::result::Result<Case, String> {
        let steps = match (self.messages, self.retain_from, self.steps) {
            (Some(messages), Some(retain_from), None) => vec![Step {
                messages,
                retain_from,
            }],
            (None, None, Some(steps)) if steps.len() == CHAIN_STEPS => steps,
            (None, None, Some(steps)) => {
                return Err(format!(
                    "steps has {} slices, a chain needs {CHAIN_STEPS}",
                    steps.len()
                ));
            }
            (None, None, None) => {
                return Err("a case needs messages and retainFrom, or steps".to_owned());
            }
            (Some(_), Some(_), Some(_)) => {
                return Err("a case has both messages and steps".to_owned());
            }
            (Some(_), None, _) | (None, Some(_), _) => {
                return Err("a case needs both messages and retainFrom".to_owned());
            }
        };
        validate_steps(&steps)?;
        Ok(Case {
            id: self.id,
            description: self.description,
            context_window: self.context_window,
            earlier_summary: self.earlier_summary,
            probes: self.probes,
            steps,
        })
    }
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Probe {
    id: String,
    kind: ProbeKind,
    text: String,
    #[serde(default)]
    item: Option<ItemClass>,
}

#[derive(Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProbeKind {
    Exact,
    Fact,
    Absent,
}

/// The 3b class a `fact` probe checks (compaction-quality §3). Exact
/// identifiers, the eighth item, are the `exact` probes (ContextCompaction
/// §13.4).
#[derive(Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
enum ItemClass {
    Task,
    Constraint,
    Decision,
    RuledOut,
    OpenItem,
    Judgment,
    NextStep,
}
impl ItemClass {
    fn name(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Constraint => "constraint",
            Self::Decision => "decision",
            Self::RuledOut => "ruledOut",
            Self::OpenItem => "openItem",
            Self::Judgment => "judgment",
            Self::NextStep => "nextStep",
        }
    }
}
const ITEMS: [ItemClass; 7] = [
    ItemClass::Task,
    ItemClass::Constraint,
    ItemClass::Decision,
    ItemClass::RuledOut,
    ItemClass::OpenItem,
    ItemClass::Judgment,
    ItemClass::NextStep,
];

fn cases_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(CASES)
}
fn read_cases(directory: &Path) -> Result<Vec<Case>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(directory)?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    files.sort();
    let mut cases = Vec::new();
    for file in files {
        let bytes = std::fs::read(&file)?;
        let raw: CaseFile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", file.display()))?;
        let case = raw
            .into_case()
            .map_err(|error| format!("{}: {error}", file.display()))?;
        validate_probes(&case.probes).map_err(|error| format!("{}: {error}", file.display()))?;
        cases.push(case);
    }
    Ok(cases)
}

/// `item` names the 3b class a `fact` probe checks; on another kind it would
/// have no meaning, because the breakdown counts `fact` probes only.
fn validate_probes(probes: &[Probe]) -> std::result::Result<(), String> {
    for probe in probes {
        if probe.item.is_some() && probe.kind != ProbeKind::Fact {
            return Err(format!(
                "probe {}: item applies to fact probes only",
                probe.id
            ));
        }
    }
    Ok(())
}

/// Each step's only cut is `retainFrom`, so it must not separate a tool call
/// from its result (ContextCompaction §4.3, §13.4); in a chain, step k must
/// repeat step k−1's retained tail verbatim, because a real compaction keeps
/// it and the next compaction sees it before the new messages.
fn validate_steps(steps: &[Step]) -> std::result::Result<(), String> {
    for (index, step) in steps.iter().enumerate() {
        validate_cut(&step.messages, step.retain_from)
            .map_err(|error| format!("step {}: {error}", index + 1))?;
        let Some(previous) = index.checked_sub(1).map(|index| &steps[index]) else {
            continue;
        };
        if !step
            .messages
            .starts_with(&previous.messages[previous.retain_from..])
        {
            return Err(format!(
                "step {} does not begin with step {}'s retained tail",
                index + 1,
                index
            ));
        }
    }
    Ok(())
}
fn validate_cut(messages: &[Message], retain_from: usize) -> std::result::Result<(), String> {
    if retain_from > messages.len() {
        return Err(format!(
            "retainFrom {retain_from} is past the {} messages",
            messages.len()
        ));
    }
    let calls = tool_uses(&messages[..retain_from]);
    for message in &messages[retain_from..] {
        for id in tool_results(std::slice::from_ref(message)) {
            if calls.contains(&id) {
                return Err(format!(
                    "retainFrom {retain_from} separates the tool call {id} from its result"
                ));
            }
        }
    }
    Ok(())
}
fn parts(message: &Message) -> &[MessagePart] {
    match &message.content {
        MessageContent::Text(_) => &[],
        MessageContent::Parts(parts) => parts,
    }
}
fn tool_uses(messages: &[Message]) -> HashSet<String> {
    messages
        .iter()
        .flat_map(parts)
        .filter_map(|part| match part {
            MessagePart::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}
fn tool_results(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(parts)
        .filter_map(|part| match part {
            MessagePart::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect()
}

/// One step of a case as the strategy sees it: the conversation request, the
/// history with its `m<i>` sources, and its single cut.
struct PreparedStep {
    request: Arc<ModelRequest>,
    history: Vec<HistoryEntry>,
    retain_from: usize,
    model: Arc<dyn ContextModel>,
}
impl PreparedStep {
    async fn new(
        step: &Step,
        selection: &ModelSelection,
        window: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        let mut request = ModelRequest::new(
            &selection.provider_id,
            &selection.model_id,
            step.messages.clone(),
        );
        request.capability_snapshot = selection.capability_snapshot.clone();
        request.config = selection.config.clone();
        if let Some(window) = window {
            let declared_output = {
                let snapshot = request
                    .capability_snapshot
                    .as_mut()
                    .ok_or("the selected model has no capability snapshot")?;
                snapshot.capabilities.context_window =
                    snapshot.capabilities.context_window.min(window);
                snapshot.capabilities.max_output_tokens as f64
            };
            let declared = request
                .config
                .as_ref()
                .and_then(|config| config.max_tokens)
                .unwrap_or(declared_output);
            request.config.get_or_insert_default().max_tokens =
                Some(declared.min(window as f64 * NARROWED_OUTPUT_SHARE));
        }
        let request = Arc::new(request);
        let model = ProductContextModel
            .resolve(request.clone(), cancel.clone())
            .await?;
        if model.context_window() - model.output_tokens() <= 0. {
            return Err(format!(
                "a window of {} leaves no input budget with {} output tokens",
                model.context_window(),
                model.output_tokens()
            )
            .into());
        }
        let history = step
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| HistoryEntry {
                id: format!("m{index}"),
                message: message.clone(),
                tool_context: message.role == Role::ToolResult,
                baseline: Arc::from([]),
                source: format!("m{index}"),
            })
            .collect();
        Ok(Self {
            request,
            history,
            retain_from: step.retain_from,
            model,
        })
    }
    fn input(&self, checkpoint: Option<CompactionCheckpoint>) -> CompactionInput {
        let retain_from = format!("m{}", self.retain_from);
        CompactionInput {
            history: self.history.clone(),
            checkpoint,
            request: self.request.clone(),
            model: self.model.clone(),
            retain_limit: Some(retain_from.clone()),
            cuts: vec![CompactionCut {
                retain_from,
                entry: self.retain_from,
                within_limit: true,
            }],
            trigger: CompactionTrigger::Manual,
            estimated_input_tokens: self.model.count_tokens(&self.request),
            baseline_source: BaselineSource::FullEstimate,
            baseline_prompt_tokens: None,
            delta_tokens: None,
            provider_budget: None,
            prior: None,
        }
    }
}

/// A case as the runner runs it: its steps, the first step's checkpoint, and
/// the window facts the strategy ran with, after the case's narrowing.
struct Prepared {
    id: String,
    description: String,
    probes: Vec<Probe>,
    steps: Vec<PreparedStep>,
    checkpoint: Option<CompactionCheckpoint>,
    window: f64,
    output: f64,
}
impl Prepared {
    async fn new(
        case: &Case,
        selection: &ModelSelection,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        let mut steps = Vec::new();
        for step in &case.steps {
            steps.push(
                PreparedStep::new(step, selection, case.context_window, cancel)
                    .await
                    .map_err(|error| format!("case {}: {error}", case.id))?,
            );
        }
        Ok(Self {
            id: case.id.clone(),
            description: case.description.clone(),
            probes: case.probes.clone(),
            checkpoint: case
                .earlier_summary
                .as_ref()
                .map(|summary| CompactionCheckpoint {
                    retain_from: "m0".into(),
                    replacement: Arc::from([summary_message(summary)]),
                    context_baseline: Arc::from([]),
                    replacement_parts: None,
                }),
            window: steps[0].model.context_window(),
            output: steps[0].model.output_tokens(),
            steps,
        })
    }
}
/// One step's compaction of the shipped strategy, with what it took.
struct RunStep {
    duration: Duration,
    outcome: std::result::Result<StrategyOutcome, HostError>,
}
impl RunStep {
    fn label(&self) -> &'static str {
        match &self.outcome {
            Ok(StrategyOutcome::Completed { .. }) => "completed",
            Ok(StrategyOutcome::Skipped { .. }) => "skipped",
            Ok(StrategyOutcome::Failed { .. }) => "failed",
            Err(_) => "failed",
        }
    }
    fn error(&self) -> Option<String> {
        match &self.outcome {
            Ok(StrategyOutcome::Failed { error, .. }) => Some(error.clone()),
            Err(HostError::Failed { message, .. }) => Some(message.clone()),
            Err(HostError::Cancelled(reason)) => Some(reason.clone()),
            Ok(_) => None,
        }
    }
    fn summary(&self) -> Option<String> {
        match &self.outcome {
            Ok(StrategyOutcome::Completed { replacement, .. }) => Some(render(replacement)),
            _ => None,
        }
    }
    /// The next chain step's checkpoint: the completed replacement itself, as
    /// a committed compaction becomes the next checkpoint (ContextCompaction
    /// §13.4).
    fn checkpoint(&self) -> Option<CompactionCheckpoint> {
        match &self.outcome {
            Ok(StrategyOutcome::Completed { replacement, .. }) => {
                let replacement: Arc<[Message]> = replacement.as_slice().into();
                Some(CompactionCheckpoint {
                    retain_from: "m0".into(),
                    replacement_parts: Some(ReplacementParts {
                        strategy: replacement.len() as u64,
                        digest: 0,
                    }),
                    replacement,
                    context_baseline: Arc::from([]),
                })
            }
            _ => None,
        }
    }
    fn details(&self) -> BTreeMap<String, Value> {
        let record = match &self.outcome {
            Ok(StrategyOutcome::Completed { diagnostics, .. })
            | Ok(StrategyOutcome::Skipped { diagnostics, .. })
            | Ok(StrategyOutcome::Failed { diagnostics, .. }) => Some(&diagnostics.record),
            Err(_) => None,
        };
        match record.map(|record| serde_json::to_value(&record.details)) {
            Some(Ok(Value::Object(details))) => details.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }
    fn diagnostics(&self) -> Option<Value> {
        let record = match &self.outcome {
            Ok(StrategyOutcome::Completed { diagnostics, .. })
            | Ok(StrategyOutcome::Skipped { diagnostics, .. })
            | Ok(StrategyOutcome::Failed { diagnostics, .. }) => Some(&diagnostics.record),
            Err(_) => None,
        };
        record.and_then(|record| serde_json::to_value(record).ok())
    }
    /// The reasoning the summarizer requests carried, as the diagnostics
    /// record it: the arm's value, or `inherit` once Provider's mapping
    /// rejected it (ContextCompaction §13.2).
    fn applied_reasoning(&self) -> Option<String> {
        self.details()
            .get("reasoning")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

/// A case's run: one compaction, or a chain whose steps stop at the first
/// that does not complete (ContextCompaction §13.4).
struct Run {
    steps: Vec<RunStep>,
    duration: Duration,
}
impl Run {
    fn label(&self) -> &'static str {
        if self.steps.iter().any(|step| step.label() == "failed") {
            "failed"
        } else if self.steps.iter().any(|step| step.label() == "skipped") {
            "skipped"
        } else {
            "completed"
        }
    }
    fn error(&self) -> Option<String> {
        self.steps.last().and_then(RunStep::error)
    }
    fn summary(&self) -> Option<String> {
        self.steps.last().and_then(RunStep::summary)
    }
    fn diagnostics(&self) -> Option<Value> {
        self.steps.last().and_then(RunStep::diagnostics)
    }
    /// The steps that ran, each with what a single run records.
    fn steps_json(&self) -> Vec<Value> {
        self.steps
            .iter()
            .map(|step| {
                json!({
                    "outcome": step.label(),
                    "error": step.error(),
                    "durationMs": step.duration.as_secs_f64() * 1000.,
                    "summary": step.summary(),
                    "diagnostics": step.diagnostics(),
                })
            })
            .collect()
    }
    /// Whether any step's summarizer fell back from the arm's reasoning value.
    fn fell_back(&self, arm: &Arm) -> bool {
        self.steps.iter().any(|step| {
            step.applied_reasoning()
                .is_some_and(|applied| applied != arm.reasoning.name())
        })
    }
    /// The configuration the summarizer's requests carried: the built one,
    /// with the reasoning the last step applied in place of it
    /// (ContextCompaction §13.4).
    fn configuration(&self, built: &Configuration) -> Configuration {
        let applied = self.steps.last().and_then(RunStep::applied_reasoning);
        built.clone().applied(applied.as_deref())
    }
    fn cut_offs(&self) -> u64 {
        self.steps
            .iter()
            .map(|step| {
                step.details()
                    .get("cutOffCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .sum()
    }
    fn tokens(&self) -> Option<f64> {
        self.steps
            .last()?
            .details()
            .get("estimatedSummaryTokens")
            .and_then(Value::as_f64)
    }
    fn target_tokens(&self) -> Option<f64> {
        self.steps
            .last()?
            .details()
            .get("targetTokens")
            .and_then(Value::as_f64)
    }
}

async fn run_case(strategy: &SummaryStrategy, case: &Prepared, cancel: &CancellationToken) -> Run {
    let started = Instant::now();
    let mut checkpoint = case.checkpoint.clone();
    let mut steps = Vec::new();
    for prepared in &case.steps {
        let step_started = Instant::now();
        let outcome = strategy
            .compact(prepared.input(checkpoint.take()), cancel.clone())
            .await;
        let step = RunStep {
            duration: step_started.elapsed(),
            outcome,
        };
        let next = step.checkpoint();
        let done = step.label() != "completed";
        steps.push(step);
        if done {
            break;
        }
        checkpoint = next;
    }
    Run {
        steps,
        duration: started.elapsed(),
    }
}

fn render(replacement: &[Message]) -> String {
    replacement
        .iter()
        .map(|message| match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One judge answer; the scenario runner reads the same shape.
#[derive(serde::Deserialize)]
pub struct Judgement {
    pub id: String,
    pub verdict: Verdict,
    #[serde(default)]
    pub quote: Option<String>,
}
#[derive(Clone, Copy, Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Present,
    Absent,
    Contradicted,
}
impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Contradicted => "contradicted",
        }
    }
}

/// What a run's judge step produced.
enum Judged {
    /// The judge answered; a probe it skipped stays `unparsed`.
    Verdicts(BTreeMap<String, (Verdict, Option<String>)>),
    /// The compaction left no summary, so there is nothing to judge; the
    /// judged probes are absent without a judge call.
    NoSummary,
    /// The judge call or its answer failed; every judged probe is `unparsed`
    /// and the evaluation goes on (ContextCompaction §13.4).
    Unavailable(String),
}
impl Judged {
    fn error(&self) -> Option<&str> {
        match self {
            Self::Unavailable(error) => Some(error),
            _ => None,
        }
    }
}

/// The probe outcomes of one run: `exact` probes are mechanical, `fact` and
/// `absent` probes are judged on the summary alone (ContextCompaction §13.4).
/// An `exact` probe is compared in the envelope's escaping, which is what the
/// summary is stored in: the probe goes through the producer's table
/// (`context_block::text`) and the summary is left alone, so there is one
/// escape table rather than an inverse of it here.
fn probe_records(probes: &[Probe], summary: &str, judged: &Judged) -> Vec<Value> {
    probes
        .iter()
        .map(|probe| {
            let (verdict, quote) = match probe.kind {
                ProbeKind::Exact => (
                    if normalize(summary).contains(&normalize(&context_block::text(&probe.text)))
                    {
                        "present"
                    } else {
                        "absent"
                    },
                    None,
                ),
                ProbeKind::Fact | ProbeKind::Absent => match judged {
                    Judged::NoSummary => ("absent", None),
                    Judged::Verdicts(verdicts) => verdicts
                        .get(&probe.id)
                        .map_or(("unparsed", None), |(verdict, quote)| {
                            (verdict.name(), quote.clone())
                        }),
                    Judged::Unavailable(_) => ("unparsed", None),
                },
            };
            json!({"id": probe.id, "kind": probe.kind, "text": probe.text, "verdict": verdict, "quote": quote})
        })
        .collect()
}

/// Judges one run's `fact` and `absent` probes; a failure is recorded, never
/// propagated, so one bad answer cannot end the evaluation. `no_temperature`
/// reports whether the judge had to drop its temperature, which is part of the
/// instrument's record (ContextCompaction §13.4).
async fn judge_run(
    host: &Arc<ModelHost>,
    resolver: &Arc<ProviderConnectionResolver>,
    judge: &ModelSelection,
    case: &Prepared,
    run: &Run,
    cancel: &CancellationToken,
    no_temperature: &mut bool,
) -> Judged {
    let Some(summary) = run.summary() else {
        return Judged::NoSummary;
    };
    if !case
        .probes
        .iter()
        .any(|probe| probe.kind != ProbeKind::Exact)
    {
        return Judged::Verdicts(BTreeMap::new());
    }
    match judge_probes(
        host,
        resolver,
        judge,
        case,
        &summary,
        cancel,
        no_temperature,
    )
    .await
    {
        Ok(verdicts) => Judged::Verdicts(verdicts),
        Err(error) => Judged::Unavailable(error),
    }
}

async fn judge_probes(
    host: &Arc<ModelHost>,
    resolver: &Arc<ProviderConnectionResolver>,
    judge: &ModelSelection,
    case: &Prepared,
    summary: &str,
    cancel: &CancellationToken,
    no_temperature: &mut bool,
) -> std::result::Result<BTreeMap<String, (Verdict, Option<String>)>, String> {
    let statements = case
        .probes
        .iter()
        .filter(|probe| probe.kind != ProbeKind::Exact)
        .map(|probe| format!("- id: {}, statement: {}", probe.id, probe.text))
        .collect::<Vec<_>>()
        .join("\n");
    let text =
        format!("<summary>\n{summary}\n</summary>\n\n<statements>\n{statements}\n</statements>");
    let mut request = ModelRequest::new(
        &judge.provider_id,
        &judge.model_id,
        vec![Message::text(Role::User, text)],
    );
    request.capability_snapshot = judge.capability_snapshot.clone();
    let mut config = judge.config.clone().unwrap_or_default();
    config.temperature = Some(0.);
    request.config = Some(config);
    request.system = Some(Arc::new(dim_agent_types::SystemPrompt::Text(
        JUDGE_SYSTEM.into(),
    )));
    let request = Arc::new(request);
    let reply = match complete(host, resolver, &request, cancel).await {
        Ok(reply) => reply,
        Err(error) => {
            let mut retry = (*request).clone();
            if retry
                .config
                .as_ref()
                .and_then(|config| config.temperature)
                .is_none()
            {
                return Err(format!("the judge failed: {error}"));
            }
            retry.config.as_mut().expect("a config").temperature = None;
            *no_temperature = true;
            match complete(host, resolver, &Arc::new(retry), cancel).await {
                Ok(reply) => reply,
                Err(retry) => return Err(format!("the judge failed: {error}, then {retry}")),
            }
        }
    };
    let parsed = parse_judgements(&reply)
        .map_err(|_| format!("the judge answered with no verdict list: {reply}"))?;
    Ok(parsed
        .into_iter()
        .map(|judgement| {
            (
                judgement.id,
                (judgement.verdict, judgement.quote.filter(|q| !q.is_empty())),
            )
        })
        .collect())
}

pub(crate) async fn complete(
    host: &Arc<ModelHost>,
    resolver: &Arc<ProviderConnectionResolver>,
    request: &Arc<ModelRequest>,
    cancel: &CancellationToken,
) -> Result<String> {
    let resolved = resolver.0.resolve(request.clone(), cancel.clone()).await?;
    let (text, _) = invoke_with_recovery(
        host,
        request,
        resolved,
        cancel,
        &mut (),
        |stream: ModelStream, _, _, _| Box::pin(collected(stream)),
    )
    .await?;
    Ok(text)
}

async fn collected(mut stream: ModelStream) -> std::result::Result<(String, bool), HostError> {
    let mut text = String::new();
    let mut stopped = false;
    while let Some(chunk) = stream.next().await {
        match chunk? {
            ModelChunk::Text(delta) => text.push_str(&delta),
            ModelChunk::MessageStop { .. } => stopped = true,
            _ => {}
        }
    }
    if !stopped {
        return Err(HostError::failed("the judge stream ended without a stop"));
    }
    Ok((text, true))
}

/// The verdicts of a judge reply: the first JSON array that parses as a
/// verdict list, or else the first run of verdict objects, one per line.
/// Prose, a fenced block or a bracketed lead-in before the verdicts must not
/// hide them, and a bracket inside a quoted verdict is part of the JSON, not
/// a boundary. A reply with neither says why it could not be read.
pub(crate) fn parse_judgements(text: &str) -> std::result::Result<Vec<Judgement>, String> {
    let mut array_error = None;
    for (start, _) in text.match_indices('[') {
        match serde_json::Deserializer::from_str(&text[start..])
            .into_iter::<Vec<Judgement>>()
            .next()
        {
            Some(Ok(judged)) => return Ok(judged),
            Some(Err(error)) => {
                array_error.get_or_insert(error.to_string());
            }
            None => {}
        }
    }
    if let Some(judged) = objects(text) {
        return Ok(judged);
    }
    Err(array_error.unwrap_or_else(|| "the reply holds no verdict object".to_owned()))
}

/// The first run of verdict objects in a reply, one per line: the objects
/// from the first `{` that starts one, up to the first value that is not one.
fn objects(text: &str) -> Option<Vec<Judgement>> {
    text.match_indices('{').find_map(|(start, _)| {
        let mut values =
            serde_json::Deserializer::from_str(&text[start..]).into_iter::<Judgement>();
        let mut judged = Vec::new();
        while let Some(Ok(judgement)) = values.next() {
            judged.push(judgement);
        }
        (!judged.is_empty()).then_some(judged)
    })
}

/// Whitespace-normalized text for the exact probes.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The configuration a summarizer or judge request carried: the reasoning
/// effort and service tier of the model config it was built with. Saved
/// preferences enter through `ProviderService::configuration`, and the
/// strategy's applied reasoning replaces the effort where its mapping took it
/// (ContextCompaction §13.4). One shape serves the arm and the judge.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Configuration {
    pub(crate) effort: Option<String>,
    pub(crate) tier: Option<String>,
}
impl Configuration {
    /// Reads the configuration a request built from `config` carries. A saved
    /// service tier lands in the OpenAI protocols' options, the only protocols
    /// the preferences support (`provider/preferences.rs`).
    pub(crate) fn read(config: Option<&ModelConfig>) -> Self {
        let Some(config) = config else {
            return Self::default();
        };
        let options = config.provider_options.as_ref();
        Self {
            effort: config
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort.clone()),
            tier: options
                .and_then(|options| options.openai.as_ref())
                .and_then(|openai| openai.service_tier.clone())
                .or_else(|| {
                    options
                        .and_then(|options| options.openai_compatible.as_ref())
                        .and_then(|compatible| compatible.service_tier.clone())
                }),
        }
    }
    /// The configuration with the reasoning the strategy applied in place of
    /// the built effort: `low` is carried as low and `off` as no reasoning,
    /// however the protocol spells that off, while `inherit` — what the
    /// diagnostics record once Provider's mapping rejected the arm's value —
    /// keeps the built one (ContextCompaction §13.2).
    fn applied(self, reasoning: Option<&str>) -> Self {
        match reasoning {
            Some("off") => Self {
                effort: Some("none".into()),
                ..self
            },
            Some("low") => Self {
                effort: Some("low".into()),
                ..self
            },
            _ => self,
        }
    }
    fn json(&self) -> Value {
        json!({"reasoningEffort": self.effort, "serviceTier": self.tier})
    }
}

/// The arm's metrics over its cases and runs (ContextCompaction §13.4).
#[derive(Default)]
struct Metrics {
    exact: (usize, usize),
    facts: (usize, usize),
    /// `fact` probes that carry an `item`, counted per 3b class
    /// (ContextCompaction §13.4).
    items: BTreeMap<ItemClass, (usize, usize)>,
    violations: usize,
    /// Judged probes the judge did not answer, for the operator to correct
    /// by hand (ContextCompaction §13.4).
    unjudged: usize,
    /// The `(case, probe)` pairs the judge answered at least once
    /// (model-prompts §8.1): the judged floor asks every scored probe for
    /// one in each arm.
    judged: BTreeSet<(String, String)>,
    cut_offs: u64,
    failed: usize,
    skipped: usize,
    /// Runs whose applied reasoning differs from the arm's value, because
    /// Provider's mapping rejected the effort for this model (ContextCompaction
    /// §13.2).
    fell_back: usize,
    /// The summarizer configurations the arm's runs carried: one, unless a
    /// run's reasoning fell back and another did not (ContextCompaction
    /// §13.4).
    configurations: BTreeSet<Configuration>,
    tokens: Vec<f64>,
    durations: Vec<f64>,
    ratios: Vec<f64>,
}
impl Metrics {
    fn add(
        &mut self,
        arm: &Arm,
        case: &Prepared,
        run: &Run,
        probes: &[Value],
        configuration: &Configuration,
    ) {
        if run.fell_back(arm) {
            self.fell_back += 1;
        }
        self.configurations.insert(configuration.clone());
        self.cut_offs += run.cut_offs();
        if let Some(tokens) = run.tokens() {
            self.tokens.push(tokens);
            if let Some(target) = run.target_tokens() {
                self.ratios.push(tokens / target);
            }
        }
        self.durations.push(run.duration.as_secs_f64() * 1000.);
        match run.label() {
            "completed" => {}
            "skipped" => self.skipped += 1,
            _ => self.failed += 1,
        }
        for (probe, record) in case.probes.iter().zip(probes) {
            let verdict = record["verdict"].as_str();
            let present = verdict == Some("present");
            if verdict == Some("unparsed") {
                self.unjudged += 1;
            } else {
                self.judged.insert((case.id.clone(), probe.id.clone()));
            }
            match probe.kind {
                ProbeKind::Exact => {
                    self.exact = count(self.exact, present);
                }
                ProbeKind::Fact => {
                    self.facts = count(self.facts, present);
                    if let Some(item) = probe.item {
                        let counts = self.items.entry(item).or_default();
                        *counts = count(*counts, present);
                    }
                }
                ProbeKind::Absent if present => self.violations += 1,
                ProbeKind::Absent => {}
            }
        }
    }
    fn row(&self, arm: &Arm) -> Value {
        let by_item: BTreeMap<&str, Value> = self
            .items
            .iter()
            .map(|(item, counts)| {
                (
                    item.name(),
                    json!({
                        "present": counts.0,
                        "total": counts.1,
                        "recall": recall(*counts),
                    }),
                )
            })
            .collect();
        json!({
            "arm": arm.id,
            "reasoning": arm.reasoning.name(),
            "reasoningEffort": self.carried(|configuration| &configuration.effort),
            "serviceTier": self.carried(|configuration| &configuration.tier),
            "fellBack": self.fell_back,
            "target": arm.target.map(|target| json!({
                "outputShare": target.output_share,
                "inputShare": target.input_share,
            })),
            "factRecall": recall(self.facts),
            "factRecallByItem": by_item,
            "exactRecall": recall(self.exact),
            "violations": self.violations,
            "unjudged": self.unjudged,
            "cutOffCount": self.cut_offs,
            "failed": self.failed,
            "skipped": self.skipped,
            "medianSummaryTokens": percentile(&self.tokens, 0.5),
            "medianDurationMs": percentile(&self.durations, 0.5),
            "medianTargetRatio": percentile(&self.ratios, 0.5),
            "p90TargetRatio": percentile(&self.ratios, 0.9),
        })
    }
    /// The values the arm's configurations carried for one member: `null`
    /// when none did, else the distinct values.
    fn carried(&self, member: fn(&Configuration) -> &Option<String>) -> Value {
        let values: Vec<&str> = self
            .configurations
            .iter()
            .filter_map(|configuration| member(configuration).as_deref())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        match values.as_slice() {
            [] => Value::Null,
            [only] => json!(only),
            _ => json!(values.join(", ")),
        }
    }
    fn line(&self, arm: &Arm) -> String {
        format!(
            "{}: fact {}/{}, exact {}/{}, violations {}, unjudged {}, cut-offs {}, failed {}, skipped {}, fell back {}",
            arm.id,
            self.facts.0,
            self.facts.1,
            self.exact.0,
            self.exact.1,
            self.violations,
            self.unjudged,
            self.cut_offs,
            self.failed,
            self.skipped,
            self.fell_back
        )
    }
}
fn count(counts: (usize, usize), present: bool) -> (usize, usize) {
    (counts.0 + usize::from(present), counts.1 + 1)
}
fn recall(counts: (usize, usize)) -> Option<f64> {
    (counts.1 > 0).then(|| counts.0 as f64 / counts.1 as f64)
}

/// The model-prompts §8.1 rule of the candidate arm against the current one
/// (ContextCompaction §13.4): each rate at least `current − δ` with
/// `δ = 0.05`, violations, failed runs and unjudged runs no higher, and every
/// scored probe judged at least once in each arm. A rate with no probe of its
/// kind among the cases is left out. There is no absolute pass line.
struct Rule {
    passes: bool,
    failures: Vec<String>,
}
fn rule(current: &Metrics, candidate: &Metrics, scored: &[(String, String)]) -> Rule {
    const DELTA: f64 = 0.05;
    let mut failures = Vec::new();
    let mut rate = |name: &str, current: (usize, usize), candidate: (usize, usize)| {
        let (Some(current), Some(candidate)) = (recall(current), recall(candidate)) else {
            return;
        };
        if candidate < current - DELTA {
            failures.push(format!(
                "{name} falls from {current:.3} to {candidate:.3} (δ = {DELTA:.2})"
            ));
        }
    };
    rate("fact recall", current.facts, candidate.facts);
    rate("exact recall", current.exact, candidate.exact);
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
    for (case, probe) in scored {
        for (arm, metrics) in [("current", current), ("candidate", candidate)] {
            if !metrics.judged.contains(&(case.clone(), probe.clone())) {
                failures.push(format!("{case}/{probe} has no judged run in the {arm} arm"));
            }
        }
    }
    Rule {
        passes: failures.is_empty(),
        failures,
    }
}

/// The verdict line: whether the candidate passes, the rules that failed
/// when it does not, and both arms' median `T` beside them, so the length is
/// visible (ContextCompaction §13.4).
fn verdict(current: &Metrics, candidate: &Metrics, scored: &[(String, String)]) -> String {
    let rule = rule(current, candidate, scored);
    let failures = if rule.failures.is_empty() {
        String::new()
    } else {
        format!(": {}", rule.failures.join("; "))
    };
    format!(
        "candidate {}{failures}; median T: current {}, candidate {}",
        if rule.passes { "passes" } else { "fails" },
        median_tokens(current),
        median_tokens(candidate),
    )
}
fn median_tokens(metrics: &Metrics) -> String {
    percentile(&metrics.tokens, 0.5).map_or_else(|| "–".to_owned(), |tokens| format!("{tokens:.0}"))
}
fn percentile(values: &[f64], at: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() as f64 * at).ceil() as usize).saturating_sub(1);
    Some(sorted[index.min(sorted.len() - 1)])
}

/// The Markdown table of every arm's metrics, with the configuration each
/// arm's summarizer requests carried beside its reasoning value
/// (ContextCompaction §13.4).
fn table(rows: &[Value]) -> String {
    let mut text = String::from(
        "| arm | reasoning | effort | service tier | fell back | target | fact recall | exact recall | violations | unjudged | cut-offs | failed | skipped | median summary tokens | median duration ms | median T/S | p90 T/S |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for row in rows {
        let number = |value: &Value| match value.as_f64() {
            Some(value) => format!("{value:.3}"),
            None => "–".to_owned(),
        };
        let carried = |value: &Value| value.as_str().unwrap_or("–").to_owned();
        let target = match row["target"]["outputShare"].as_f64() {
            Some(output) => format!(
                "{}/{}",
                output,
                row["target"]["inputShare"].as_f64().unwrap_or_default()
            ),
            None => "none".to_owned(),
        };
        text += &format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            row["arm"].as_str().unwrap_or_default(),
            row["reasoning"].as_str().unwrap_or_default(),
            carried(&row["reasoningEffort"]),
            carried(&row["serviceTier"]),
            row["fellBack"],
            target,
            number(&row["factRecall"]),
            number(&row["exactRecall"]),
            row["violations"],
            row["unjudged"],
            row["cutOffCount"],
            row["failed"],
            row["skipped"],
            number(&row["medianSummaryTokens"]),
            number(&row["medianDurationMs"]),
            number(&row["medianTargetRatio"]),
            number(&row["p90TargetRatio"]),
        );
    }
    if let Some(items) = item_table(rows) {
        text.push('\n');
        text.push_str(&items);
    }
    text
}

/// The judge the evaluation used. It is the instrument: one configuration per
/// evaluation, so two evaluations compare only when these match
/// (ContextCompaction §13.4).
fn judge_line(
    judge: &ModelId,
    configuration: &Configuration,
    no_temperature: usize,
    runs: usize,
) -> String {
    format!(
        "\njudge: {}/{}, reasoning effort {}, service tier {}, temperature dropped in {no_temperature} of {runs} runs\n",
        judge.provider_id,
        judge.model_id,
        configuration.effort.as_deref().unwrap_or("none"),
        configuration.tier.as_deref().unwrap_or("none"),
    )
}

/// The candidate texts' own token cost beside the scores (model-prompts
/// §8.1): the prompt, and the length sentence when one is given, counted
/// with the product's counter on the request each text travels in. Empty
/// without a candidate: the current arm runs the shipped texts.
fn candidate_cost_line(
    model: &ModelId,
    prompt: Option<&str>,
    length_sentence: Option<&LengthSentence>,
) -> String {
    let Some(prompt) = prompt else {
        return String::new();
    };
    let mut line = format!(
        "\ncandidate tokens: prompt {:.0}",
        text_tokens(model, prompt)
    );
    if let Some(sentence) = length_sentence {
        line += &format!(
            ", length sentence {:.0}",
            text_tokens(model, sentence.as_str())
        );
    }
    line.push('\n');
    line
}

/// `F` per 3b class: one row per `item` a `fact` probe names, one column per
/// arm (ContextCompaction §13.4).
fn item_table(rows: &[Value]) -> Option<String> {
    let items: Vec<ItemClass> = ITEMS
        .into_iter()
        .filter(|item| {
            rows.iter()
                .any(|row| !row["factRecallByItem"][item.name()].is_null())
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let mut text = String::from("| item |");
    for row in rows {
        text += &format!(" {} |", row["arm"].as_str().unwrap_or_default());
    }
    text.push_str("\n| --- |");
    for _ in rows {
        text.push_str(" --- |");
    }
    text.push('\n');
    for item in items {
        text += &format!("| {} |", item.name());
        for row in rows {
            let recall = row["factRecallByItem"][item.name()]["recall"].as_f64();
            let cell = recall.map_or_else(|| "–".to_owned(), |recall| format!("{recall:.3}"));
            text += &format!(" {cell} |");
        }
        text.push('\n');
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dim_agent_capabilities::host::{
        HostFuture, ModelConnection, ModelDriver, ModelStopReason, ResolveModelConnection,
        ResolvedModelConnection,
    };
    use dim_agent_types::ModelCapabilitySnapshot;
    use dim_app_server::model_catalog::FieldUpdate;
    use dim_app_server::provider::{CreateProvider, PreferencesPatch};
    use std::sync::Mutex;

    /// The verdicts are read from a JSON array, with prose and a bracketed
    /// lead-in before it and a bracket inside a quote.
    #[test]
    fn a_verdict_array_is_read_with_prose_around_it() {
        let judged = parse_judgements(
            "The verdicts are:\n```json\n[not this]\n\
             [{\"id\": \"ran\", \"verdict\": \"present\", \"quote\": \"turn/end]\"}]\n\
             ```\nThat is all.",
        )
        .unwrap();
        assert_eq!(judged.len(), 1);
        assert_eq!(judged[0].id, "ran");
        assert_eq!(judged[0].verdict, Verdict::Present);
    }

    /// The verdicts are read from one object per line, with prose around the
    /// run and a brace in the prose before it.
    #[test]
    fn verdicts_one_object_per_line_are_read_with_prose_around_them() {
        let judged = parse_judgements(
            "Here they are, brace {not json} and all:\n\
             {\"id\": \"ran\", \"verdict\": \"present\", \"quote\": \"turn/end\"}\n\
             {\"id\": \"floods\", \"verdict\": \"absent\", \"quote\": \"\"}\n\
             That is all.",
        )
        .unwrap();
        assert_eq!(judged.len(), 2);
        assert_eq!(judged[0].id, "ran");
        assert_eq!(judged[0].verdict, Verdict::Present);
        assert_eq!(judged[1].id, "floods");
        assert_eq!(judged[1].verdict, Verdict::Absent);
    }

    /// A reply with neither a verdict array nor a verdict object says why: the
    /// array attempt's own error where there is an array, and the missing
    /// object where there is none.
    #[test]
    fn a_reply_with_no_verdict_says_why() {
        let error = parse_judgements("the run looks fine to me")
            .err()
            .expect("a reply with no verdict object");
        assert_eq!(error, "the reply holds no verdict object");
        let error = parse_judgements(r#"[{"id": "ran", "verdict": "maybe"}]"#)
            .err()
            .expect("an array of no verdict object");
        assert!(error.contains("unknown variant `maybe`"), "{error}");
    }

    /// An arm's metrics over one judged case, with the given unjudged count
    /// and every other metric level.
    fn metrics(judged: &[(&str, &str)], unjudged: usize) -> Metrics {
        Metrics {
            judged: judged
                .iter()
                .map(|(case, probe)| ((*case).to_owned(), (*probe).to_owned()))
                .collect(),
            unjudged,
            ..Metrics::default()
        }
    }
    fn scored_probes(probes: &[(&str, &str)]) -> Vec<(String, String)> {
        probes
            .iter()
            .map(|(case, probe)| ((*case).to_owned(), (*probe).to_owned()))
            .collect()
    }

    /// More unjudged probes than the current arm fails the verdict even when
    /// every other metric is level (model-prompts §8.1).
    #[test]
    fn a_candidate_with_more_unjudged_runs_fails() {
        let scored = scored_probes(&[("cjk", "constraint")]);
        let current = metrics(&[("cjk", "constraint")], 0);
        let candidate = metrics(&[("cjk", "constraint")], 1);
        assert_eq!(
            verdict(&current, &candidate, &scored),
            "candidate fails: unjudged runs rise from 0 to 1; median T: current –, candidate –"
        );
        assert!(verdict(&current, &current, &scored).starts_with("candidate passes"));
    }

    /// A scored probe the judge never answered in one arm fails the verdict
    /// (model-prompts §8.1): the rate it feeds would rest on no judgment
    /// there.
    #[test]
    fn a_probe_with_no_judged_run_in_one_arm_fails() {
        let both = [("cjk", "constraint"), ("code", "signature")];
        let scored = scored_probes(&both);
        let line = verdict(
            &metrics(&both, 0),
            &metrics(&[("cjk", "constraint")], 0),
            &scored,
        );
        assert!(line.starts_with("candidate fails"), "{line}");
        assert!(
            line.contains("code/signature has no judged run in the candidate arm"),
            "{line}"
        );
        let line = verdict(
            &metrics(&[("code", "signature")], 0),
            &metrics(&both, 0),
            &scored,
        );
        assert!(
            line.contains("cjk/constraint has no judged run in the current arm"),
            "{line}"
        );
    }

    /// The report states the candidate texts' own token cost beside the
    /// scores, counted as the product counts the request each text travels in
    /// (model-prompts §8.1): the prompt, and the length sentence too when one
    /// is given.
    #[test]
    fn the_report_states_the_candidate_texts_token_cost() {
        let model = ModelId {
            provider_id: "provider".into(),
            model_id: "model".into(),
        };
        let prompt = "You write summaries of coding sessions.";
        let sentence = LengthSentence::new("Keep it to {target} tokens.").unwrap();
        assert_eq!(
            candidate_cost_line(&model, Some(prompt), Some(&sentence)),
            format!(
                "\ncandidate tokens: prompt {:.0}, length sentence {:.0}\n",
                text_tokens(&model, prompt),
                text_tokens(&model, sentence.as_str())
            )
        );
        assert_eq!(
            candidate_cost_line(&model, Some(prompt), None),
            format!(
                "\ncandidate tokens: prompt {:.0}\n",
                text_tokens(&model, prompt)
            )
        );
        assert_eq!(candidate_cost_line(&model, None, None), "");
    }

    /// The conversation request carries the model's saved preferences, as the
    /// bundled client's `session.create` does; without saved preferences it
    /// stays the model config alone (ContextCompaction §13.4). The run record
    /// reports the same reasoning effort and service tier, and a provider the
    /// catalog does not hold is an error.
    #[tokio::test]
    async fn summary_eval_selection_carries_saved_model_preferences() {
        let dir = crate::app_server::scratch("summary-eval-preferences").unwrap();
        let server = crate::app_server::serve_product(&dir, |_| {})
            .await
            .unwrap();
        let providers = server
            .service::<ProviderService>()
            .expect("the product profile serves a provider catalog");
        let cancel = CancellationToken::new();
        let provider_id = "00000000-0000-4000-8000-0000000000aa";
        let created = providers
            .create(
                serde_json::from_value::<CreateProvider>(json!({
                    "providerId": provider_id,
                    "displayName": "local",
                    "connection": {
                        "driverKind": "openai-compatible",
                        "auth": {"type": "anonymous"},
                        "baseUrl": "http://127.0.0.1:1"
                    },
                    "models": [{
                        "modelId": "deepseek",
                        "displayName": "DeepSeek",
                        "status": "active",
                        "capabilities": {
                            "modalities": ["text"], "toolCalling": true, "streaming": true,
                            "structuredOutput": true, "vision": false, "audio": false,
                            "reasoning": true, "contextWindow": 128000, "maxOutputTokens": 321
                        },
                        "metadata": {
                            "reasoning": {"supported": true, "effortOptions": ["low", "high", "max"]},
                            "serviceTier": {"supported": true, "options": ["flex"]}
                        }
                    }],
                    "enabled": true
                }))
                .unwrap(),
                &cancel,
            )
            .await
            .unwrap();
        let target = ModelId {
            provider_id: provider_id.into(),
            model_id: "deepseek".into(),
        };
        let plain = selection_with_preferences(&providers, &target, &cancel)
            .await
            .unwrap()
            .config
            .expect("a conversation config");
        assert_eq!(plain.max_tokens, Some(321.));
        assert_eq!(plain.reasoning, None);
        assert_eq!(
            Configuration::read(Some(&plain)).json(),
            json!({"reasoningEffort": null, "serviceTier": null})
        );
        providers
            .update_preferences(
                provider_id.into(),
                created.version.expect("a provider version"),
                "deepseek".into(),
                PreferencesPatch {
                    reasoning_effort: Some(FieldUpdate::Set {
                        value: "max".into(),
                    }),
                    service_tier: Some(FieldUpdate::Set {
                        value: "flex".into(),
                    }),
                },
                &cancel,
            )
            .await
            .unwrap();
        let saved = selection_with_preferences(&providers, &target, &cancel)
            .await
            .unwrap()
            .config
            .expect("a conversation config");
        assert_eq!(saved.max_tokens, Some(321.));
        assert_eq!(
            saved
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort.clone()),
            Some("max".into())
        );
        assert_eq!(
            saved
                .provider_options
                .as_ref()
                .and_then(|options| options.openai_compatible.as_ref())
                .and_then(|compatible| compatible.service_tier.clone()),
            Some("flex".into())
        );
        assert_eq!(
            Configuration::read(Some(&saved)).json(),
            json!({"reasoningEffort": "max", "serviceTier": "flex"})
        );
        let absent = selection_with_preferences(
            &providers,
            &ModelId {
                provider_id: "00000000-0000-4000-8000-0000000000bb".into(),
                model_id: "deepseek".into(),
            },
            &cancel,
        )
        .await;
        let Err(absent) = absent else {
            panic!("a provider the catalog lacks is an error, not a default selection");
        };
        assert!(
            absent
                .to_string()
                .contains("00000000-0000-4000-8000-0000000000bb"),
            "the error names the provider: {absent}"
        );
        server.shutdown();
        server.join().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every committed case parses and validates without a model call.
    #[test]
    fn summary_eval_cases_parse_and_validate() {
        let cases = read_cases(&cases_directory()).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            cases.iter().any(|case| case.id == "chain"),
            "the chain case is present"
        );
    }

    /// Answers each summarizer request with `summary-of-step-N`, N being the
    /// request's position, and keeps every request.
    #[derive(Default)]
    struct Markers {
        requests: Mutex<Vec<Arc<ModelRequest>>>,
    }
    impl ModelDriver for Markers {
        fn invoke(
            &self,
            request: Arc<ModelRequest>,
            _: ModelConnection,
            _: CancellationToken,
        ) -> HostFuture<'_, ModelStream> {
            let text = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request);
                format!("summary-of-step-{}", requests.len())
            };
            Box::pin(async move {
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(ModelChunk::MessageStart {
                        message_id: "reply".into(),
                    }),
                    Ok(ModelChunk::Text(text)),
                    Ok(ModelChunk::MessageStop {
                        message_id: "reply".into(),
                        reason: ModelStopReason::EndTurn,
                    }),
                ])) as ModelStream)
            })
        }
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

    /// The model the strategy tests run on: one target, its capabilities
    /// known, and no configuration of its own.
    fn fake_selection() -> ModelSelection {
        let snapshot: ModelCapabilitySnapshot = serde_json::from_value(json!({
            "target": {"providerId": "fake", "modelId": "fake"},
            "capabilities": {"modalities": ["text"], "toolCalling": true, "streaming": true,
                "structuredOutput": false, "vision": false, "audio": false, "reasoning": false,
                "contextWindow": 128_000, "maxOutputTokens": 16_384}
        }))
        .unwrap();
        ModelSelection {
            provider_id: "fake".into(),
            model_id: "fake".into(),
            capability_snapshot: Some(snapshot),
            config: None,
            metadata: None,
        }
    }

    /// A chain hands each step the previous step's completed replacement
    /// unchanged — one summary envelope around one marker, the way a
    /// committed compaction becomes the next checkpoint (ContextCompaction
    /// §13.4).
    #[tokio::test]
    async fn a_chain_carries_each_completed_replacement_unchanged() {
        let cases = read_cases(&cases_directory()).unwrap();
        let case = cases
            .iter()
            .find(|case| case.id == "chain")
            .expect("the chain case");
        let driver = Arc::new(Markers::default());
        let strategy = SummaryStrategy::new(
            Arc::new(ModelHost(driver.clone())),
            Arc::new(ProviderConnectionResolver(Arc::new(Local))),
            ProductSummaryStrategyConfig::default(),
        );
        let selection = fake_selection();
        let cancel = CancellationToken::new();
        let prepared = Prepared::new(case, &selection, &cancel).await.unwrap();
        let run = run_case(&strategy, &prepared, &cancel).await;
        assert_eq!(run.steps.len(), CHAIN_STEPS, "every step completed");
        assert!(
            run.steps.iter().all(|step| step.label() == "completed"),
            "{:?}",
            run.steps.iter().map(RunStep::label).collect::<Vec<_>>()
        );
        let requests = driver.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), CHAIN_STEPS, "one chunk per step");
        for (index, request) in requests.iter().enumerate().skip(1) {
            let text = request
                .messages
                .iter()
                .map(|message| render(std::slice::from_ref(message.as_ref())))
                .collect::<Vec<_>>()
                .join("\n");
            let marker = format!("summary-of-step-{index}");
            assert_eq!(
                text.matches(&marker).count(),
                1,
                "step {} does not carry {marker} exactly once:\n{text}",
                index + 1
            );
            assert_eq!(
                text.matches("<dim-summary>").count(),
                1,
                "step {} does not carry one summary envelope:\n{text}",
                index + 1
            );
            assert_eq!(
                text.matches("&lt;dim-summary&gt;").count(),
                0,
                "step {} carries an escaped summary envelope:\n{text}",
                index + 1
            );
        }
        for (index, step) in run.steps.iter().enumerate() {
            let marker = summary_message(&format!("summary-of-step-{}", index + 1));
            assert_eq!(
                step.summary(),
                Some(render(&[marker])),
                "the report and the judge read the completed replacement"
            );
        }
    }

    /// An `exact` probe matches the summary as the model wrote it: the stored
    /// replacement is the escaped envelope, so the probe is escaped with the
    /// producer's table (`&`, `<`, `>`, and no other character) and the
    /// summary is left alone (ContextCompaction §13.4).
    #[tokio::test]
    async fn exact_probes_match_the_text_the_model_wrote() {
        struct Reply;
        impl ModelDriver for Reply {
            fn invoke(
                &self,
                _: Arc<ModelRequest>,
                _: ModelConnection,
                _: CancellationToken,
            ) -> HostFuture<'_, ModelStream> {
                let text = "The runner returns Box<dyn Error>, keeps a && b, and reads \"--flag\" as a quote.";
                Box::pin(async move {
                    Ok(Box::pin(futures_util::stream::iter(vec![
                        Ok(ModelChunk::MessageStart {
                            message_id: "reply".into(),
                        }),
                        Ok(ModelChunk::Text(text.into())),
                        Ok(ModelChunk::MessageStop {
                            message_id: "reply".into(),
                            reason: ModelStopReason::EndTurn,
                        }),
                    ])) as ModelStream)
                })
            }
        }
        let cases = read_cases(&cases_directory()).unwrap();
        let case = cases
            .iter()
            .find(|case| case.id == "code")
            .expect("the code case");
        let driver = Arc::new(Reply);
        let strategy = SummaryStrategy::new(
            Arc::new(ModelHost(driver)),
            Arc::new(ProviderConnectionResolver(Arc::new(Local))),
            ProductSummaryStrategyConfig::default(),
        );
        let selection = fake_selection();
        let cancel = CancellationToken::new();
        let prepared = Prepared::new(case, &selection, &cancel).await.unwrap();
        let run = run_case(&strategy, &prepared, &cancel).await;
        let summary = run.summary().expect("the reply is a summary");
        assert!(
            !summary.contains("Box<dyn Error>") && !summary.contains("a && b"),
            "the envelope escaped the summary: {summary}"
        );
        let probes = vec![
            Probe {
                id: "error-type".into(),
                kind: ProbeKind::Exact,
                text: "Box<dyn Error>".into(),
                item: None,
            },
            Probe {
                id: "and".into(),
                kind: ProbeKind::Exact,
                text: "a && b".into(),
                item: None,
            },
            Probe {
                id: "quote".into(),
                kind: ProbeKind::Exact,
                text: "reads \"--flag\" as a quote".into(),
                item: None,
            },
        ];
        let records = probe_records(&probes, &summary, &Judged::NoSummary);
        assert_eq!(records[0]["verdict"], "present", "{records:?}");
        assert_eq!(records[1]["verdict"], "present", "{records:?}");
        assert_eq!(records[2]["verdict"], "present", "{records:?}");
    }
}

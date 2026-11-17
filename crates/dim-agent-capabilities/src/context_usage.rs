use crate::{
    context_preparation::{ContextOpError, ContextUsage as UsageRead},
    host::{ContextContribution as Contribution, ContextModel, ContextModelHost, HostError},
    tool_registry::ToolRegistry,
};
use dim_agent_runtime::{
    AgentError, BranchLifecycle, Dispatcher, Names, QueryFilter, QueryOperand, QueryOutput,
    QueryPage, QueryPick, QuerySelect, QuerySpec, QueryValue, Result, Timeline, TimelineQuery,
    TimelineQueryScope,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::{
    Ctx,
    host::{BoxFuture, CtxHost, DynPlugin, Effect, Key as ServiceKey, TypeKey},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    num::NonZeroU64,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{Notify, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

mod legacy;
mod numeric;
mod sources;
use numeric::{Entry, Numeric, NumericResult, Summary};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct Estimate {
    pub input: f64,
    pub source: BaselineSource,
    pub baseline: Option<f64>,
    pub delta: Option<f64>,
}

#[derive(Clone)]
pub(crate) struct UsageEvidence {
    base: sources::RequestBase,
    estimate: Estimate,
    limits: Limits,
    pub boundary: Option<LastEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LastEntry {
    at: Key,
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    pub id: Option<String>,
}

#[derive(Clone)]
pub(crate) struct PreparedEstimate {
    estimate: Estimate,
    algorithm: Option<String>,
    limits: Limits,
}

impl PreparedEstimate {
    pub(crate) fn new(estimate: Estimate, model: &dyn ContextModel) -> Self {
        Self {
            estimate,
            algorithm: model
                .contribution_counter()
                .map(|counter| counter.algorithm().to_owned()),
            limits: Limits {
                window: model.context_window(),
                output: model.output_tokens(),
                threshold: model.threshold_tokens(),
            },
        }
    }

    pub(crate) fn estimate(&self, model: &dyn ContextModel) -> Option<Estimate> {
        (self.algorithm.is_some()
            && self.algorithm.as_deref()
                == model
                    .contribution_counter()
                    .map(|counter| counter.algorithm())
            && self.limits
                == (Limits {
                    window: model.context_window(),
                    output: model.output_tokens(),
                    threshold: model.threshold_tokens(),
                }))
        .then(|| self.estimate.clone())
    }
}

impl UsageEvidence {
    pub(crate) fn estimate(&self, model: &dyn ContextModel) -> Option<Estimate> {
        (model
            .contribution_counter()
            .is_some_and(|counter| counter.algorithm() == self.base.algorithm)
            && self.limits
                == (Limits {
                    window: model.context_window(),
                    output: model.output_tokens(),
                    threshold: model.threshold_tokens(),
                }))
        .then(|| self.estimate.clone())
    }
}

const RESULT: &str = "context-usage/recovered";
const SCHEMA: &str = "context-usage/v5";
const COVERED: &str = "context-usage/covered";
const SOURCES: &[&str] = &[
    "context/configured",
    "input/consumed",
    "step/model-output-end",
    "step/tool-result",
    "context/compaction",
    "model/usage",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    branch: u32,
    sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    window: f64,
    output: f64,
    threshold: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CounterConfiguration {
    event_id: String,
    request_identity: String,
    #[serde(default)]
    control_identity: Option<String>,
    #[serde(default)]
    configured_tools: Option<bool>,
    algorithm: String,
    fixed: Contribution,
    limits: Limits,
}

impl CounterConfiguration {
    fn identity(&self) -> Result<String> {
        fingerprint(&(&self.request_identity, &self.limits))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Configuration {
    Ready { counter: Box<CounterConfiguration> },
    Unsupported { event_id: String },
    Unavailable { event_id: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Compression {
    event_id: String,
    cut: Key,
    removed: Contribution,
    replacement: Contribution,
    #[serde(default)]
    removed_messages: Option<u64>,
    #[serde(default)]
    replacement_messages: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Calibration {
    request_id: String,
    request_at: Key,
    anchor_at: Key,
    configuration: String,
    compression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_identity: Option<String>,
    local: f64,
    prompt_tokens: f64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    epoch: String,
    branches: Vec<String>,
    initialized: Key,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_result: Option<String>,
    current: Option<NumericResult>,
}

impl State {
    fn key(&self, position: &TimelinePosition) -> Result<Key> {
        let branch = self
            .branches
            .iter()
            .position(|branch| *branch == position.branch_id)
            .ok_or_else(|| fault("source is outside the numeric branch chain"))?;
        Ok(Key {
            branch: branch as u32,
            sequence: position.sequence,
        })
    }

    fn at(&self, sequence: u64) -> Key {
        Key {
            branch: self.branches.len() as u32 - 1,
            sequence,
        }
    }

    fn summary(&self) -> Summary {
        self.current
            .as_ref()
            .map_or_else(Summary::default, |root| root.totals.clone())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recovered {
    schema: String,
    source_through: u64,
    #[serde(flatten)]
    state: State,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Covered {
    schema: String,
    result_id: String,
    source_through: u64,
}

struct Machine {
    state: State,
    source_through: u64,
    result: u64,
    result_id: Option<String>,
    saved_through: u64,
    relevant_through: u64,
    model: Option<Arc<dyn ContextModel>>,
    unavailable: Option<HostError>,
    needs_history: bool,
    empty: (Contribution, f64),
    numeric: Numeric,
}

#[derive(Clone, Default)]
struct Progress {
    state: Option<Arc<State>>,
    covered: u64,
    relevant_through: u64,
    saved_through: u64,
    unavailable: Option<HostError>,
    needs_history: bool,
    model: Option<Arc<dyn ContextModel>>,
    error: Option<AgentError>,
}

pub(crate) struct ContextUsage {
    timeline: Arc<Timeline>,
    resolver: Arc<ContextModelHost>,
    registry: Arc<ToolRegistry>,
    dynamic_fixed: Mutex<Option<DynamicFixed>>,
    dispatcher: Arc<Dispatcher>,
    branch: Arc<BranchLifecycle>,
    wanted: AtomicU64,
    handoff: AtomicU64,
    wake: Notify,
    progress: watch::Sender<Progress>,
    tasks: TaskTracker,
    cancel: CancellationToken,
    errors: Mutex<Vec<AgentError>>,
    association: Mutex<Association>,
    initial_history: Mutex<Option<InitialHistory>>,
}

pub(crate) struct InitialHistory {
    through: u64,
    algorithm: String,
    summary: Summary,
}

#[derive(Clone)]
struct DynamicFixed {
    configuration: String,
    versions: String,
    identity: String,
    fixed: Contribution,
}

#[derive(Default)]
struct Association {
    prepared: Option<PreparedSource>,
    call: Option<Arc<CallSource>>,
    measurements: std::collections::BTreeMap<u64, Arc<CallSource>>,
}

struct RequestCalibration {
    configuration: String,
    compression: Option<String>,
    request_identity: Option<String>,
    algorithm: String,
    local: f64,
}

struct PreparedSource {
    request_id: String,
    through: u64,
    request: std::sync::Weak<ModelRequest>,
    configuration: Option<PromptReference>,
    prompt: Option<PromptReference>,
    calibration: RequestCalibration,
}

struct CallSource {
    request_id: String,
    source: TimelinePosition,
    anchor: TimelinePosition,
    calibration: RequestCalibration,
}

fn fault(message: impl Into<String>) -> AgentError {
    AgentError::fault("ContextUsage", message)
}

fn add(left: Contribution, right: Contribution) -> Result<Contribution> {
    Ok(Contribution {
        serialized_bytes: left
            .serialized_bytes
            .checked_add(right.serialized_bytes)
            .ok_or_else(|| fault("numeric byte contribution overflow"))?,
        images: left
            .images
            .checked_add(right.images)
            .ok_or_else(|| fault("numeric image contribution overflow"))?,
    })
}

fn subtract(left: Contribution, right: Contribution) -> Result<Contribution> {
    Ok(Contribution {
        serialized_bytes: left
            .serialized_bytes
            .checked_sub(right.serialized_bytes)
            .ok_or_else(|| fault("numeric byte prefix exceeds its total"))?,
        images: left
            .images
            .checked_sub(right.images)
            .ok_or_else(|| fault("numeric image prefix exceeds its total"))?,
    })
}

fn total(summary: &Summary) -> Result<Contribution> {
    let messages = match &summary.compression {
        Some(compression) => add(
            subtract(summary.ordinary, compression.removed)?,
            compression.replacement,
        )?,
        None => summary.ordinary,
    };
    match &summary.configuration {
        Some(Configuration::Ready { counter }) => add(counter.fixed, messages),
        _ => Ok(messages),
    }
}

fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
    let mut hash = Sha256::new();
    serde_json::to_writer(&mut hash, value).map_err(|error| fault(error.to_string()))?;
    Ok(format!("{:x}", hash.finalize()))
}

fn control_identity(request: &ModelRequest, algorithm: &str) -> Result<String> {
    fingerprint(&(
        &request.provider_id,
        &request.model_id,
        &request.capability_snapshot,
        &request.config,
        request
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.startup.as_ref()),
        request
            .metadata
            .as_ref()
            .map(|metadata| &metadata.extensions)
            .filter(|extensions| !extensions.is_empty()),
        algorithm,
    ))
}

fn request_identity(request: &ModelRequest, algorithm: &str) -> Result<String> {
    fingerprint(&(
        control_identity(request, algorithm)?,
        fingerprint(&(&request.system, &request.tools))?,
    ))
}

fn request(
    scope: TimelineQueryScope,
    through: u64,
    kinds: &[&str],
    fields: &[&str],
) -> TimelineQuery {
    TimelineQuery {
        scope,
        from: None,
        through: Some(through),
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::In {
                value: QueryOperand::Field {
                    path: "kind".into(),
                },
                values: kinds.iter().map(|kind| (*kind).into()).collect(),
            }),
            select: QuerySelect::Fields {
                fields: fields.iter().map(|field| (*field).into()).collect(),
            },
            group_by: vec![],
            pick: None,
            order_by: vec![],
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(128).unwrap(),
            after: None,
        }),
    }
}

fn estimation<'a>(
    summary: &'a Summary,
    request: Option<(f64, &str)>,
) -> std::result::Result<(Estimate, &'a CounterConfiguration), ContextOpError> {
    let configuration = match &summary.configuration {
        Some(Configuration::Ready { counter }) => counter,
        Some(Configuration::Unsupported { .. }) => {
            return Err(ContextOpError::Model(HostError::failed(
                "The context model does not support persistent usage statistics",
            )));
        }
        Some(Configuration::Unavailable { message, .. }) => {
            return Err(ContextOpError::Model(HostError::failed(message)));
        }
        None => return Err(ContextOpError::NotConfigured),
    };
    let local = match request {
        Some((local, _)) => local,
        None => summary
            .local
            .ok_or_else(|| ContextOpError::Read(fault("missing local estimate")))?,
    };
    let identity = configuration.identity().map_err(ContextOpError::Read)?;
    let compatible = summary.calibration.as_ref().filter(|baseline| {
        baseline.configuration == identity
            && baseline.request_identity.as_deref() == request.map(|(_, identity)| identity)
            && baseline.compression.as_deref()
                == summary
                    .compression
                    .as_ref()
                    .map(|compression| compression.event_id.as_str())
    });
    let (input, source) = match compatible {
        Some(baseline) => (
            baseline.prompt_tokens + local - baseline.local,
            BaselineSource::ProviderBaseline,
        ),
        None => (local, BaselineSource::FullEstimate),
    };
    if !input.is_finite() || input < 0. {
        return Err(ContextOpError::Read(fault(
            "invalid calibrated input estimate",
        )));
    }
    Ok((
        Estimate {
            input,
            source,
            baseline: compatible.map(|baseline| baseline.prompt_tokens),
            delta: compatible.map(|baseline| local - baseline.local),
        },
        configuration,
    ))
}

fn reading(
    through: u64,
    summary: &Summary,
    request: Option<(f64, &str)>,
) -> std::result::Result<UsageRead, ContextOpError> {
    let (estimate, configuration) = estimation(summary, request)?;
    Ok(UsageRead {
        through,
        context_window: configuration.limits.window,
        output_tokens: configuration.limits.output,
        threshold_tokens: configuration.limits.threshold,
        estimated_input_tokens: estimate.input,
        baseline_source: estimate.source,
    })
}

impl ContextUsage {
    pub(crate) async fn count_initial_history(
        &self,
        through: u64,
        history: &[crate::history::HistoryEntry],
        source: &crate::history::StreamedContext,
        request: &ModelRequest,
        model: &dyn ContextModel,
        cancel: CancellationToken,
    ) -> std::result::Result<Option<(InitialHistory, Estimate)>, HostError> {
        let _ = self.at(0, cancel).await;
        if !self.progress.borrow().needs_history {
            return Ok(None);
        }
        let Some(counter) = model.contribution_counter() else {
            return Ok(None);
        };
        let mut summary = Summary::default();
        let key = |position: &TimelinePosition| -> Result<Key> {
            Ok(Key {
                branch: source
                    .branches
                    .iter()
                    .position(|branch| *branch == position.branch_id)
                    .ok_or_else(|| fault("initial usage source outside branch chain"))?
                    as u32,
                sequence: position.sequence,
            })
        };
        let empty = counter.fixed(None, None);
        for entry in history {
            sources::message(
                &mut summary,
                counter,
                (empty, counter.estimate(empty)),
                &entry.message,
            )
            .map_err(|error| HostError::failed(error.to_string()))?;
            summary.last_entry = Some(LastEntry {
                at: key(&entry.position).map_err(|error| HostError::failed(error.to_string()))?,
                id: Some(entry.id.clone()),
            });
        }
        if let Some(event) = source.compaction.as_deref() {
            let EventKind::ContextCompaction(data) = &event.kind else {
                unreachable!("completed compaction")
            };
            let ContextCompactionResult::Completed {
                replacement,
                context_baseline,
                ..
            } = &data.outcome
            else {
                unreachable!("completed compaction")
            };
            let mut contribution = Contribution::default();
            for message in replacement
                .iter()
                .chain(context_baseline.iter().map(|entry| entry.message.as_ref()))
            {
                contribution = add(contribution, counter.message(message))
                    .map_err(|error| HostError::failed(error.to_string()))?;
            }
            summary.compression = Some(Compression {
                event_id: event.id.clone(),
                cut: key(source
                    .compaction_cut
                    .as_ref()
                    .expect("resolved compaction cut"))
                .map_err(|error| HostError::failed(error.to_string()))?,
                removed: Contribution::default(),
                replacement: contribution,
                removed_messages: Some(0),
                replacement_messages: Some((replacement.len() + context_baseline.len()) as u64),
            });
        }
        let fixed = counter.fixed(
            request.system.as_deref(),
            request.tools.as_deref().map(Vec::as_slice),
        );
        let input = counter.estimate(
            add(
                fixed,
                total(&summary).map_err(|error| HostError::failed(error.to_string()))?,
            )
            .map_err(|error| HostError::failed(error.to_string()))?,
        );
        Ok(Some((
            InitialHistory {
                through,
                algorithm: counter.algorithm().to_owned(),
                summary,
            },
            Estimate {
                input,
                source: BaselineSource::FullEstimate,
                baseline: None,
                delta: None,
            },
        )))
    }

    pub(crate) async fn initialize_counted(
        &self,
        initial: InitialHistory,
        cancel: CancellationToken,
    ) -> std::result::Result<bool, HostError> {
        if !self.progress.borrow().needs_history {
            return Ok(false);
        }
        let through = initial.through;
        *self.initial_history.lock().unwrap() = Some(initial);
        self.wake.notify_one();
        let mut progress = self.progress.subscribe();
        loop {
            let view = progress.borrow_and_update().clone();
            if let Some(error) = view.error {
                return Err(HostError::failed(error.to_string()));
            }
            if !view.needs_history {
                break;
            }
            tokio::select! {
                result = progress.changed() => result.map_err(|_| HostError::Cancelled("Usage owner closed".into()))?,
                _ = cancel.cancelled() => return Err(HostError::Cancelled("Usage initialization cancelled".into())),
                _ = self.cancel.cancelled() => return Err(HostError::Cancelled("Usage owner closed".into())),
            }
        }
        match self.at(through, cancel).await {
            Ok(_)
            | Err(
                ContextOpError::NotConfigured | ContextOpError::NotReady | ContextOpError::Model(_),
            ) => Ok(true),
            Err(ContextOpError::Read(error)) => Err(HostError::failed(error.to_string())),
        }
    }
    fn associate(&self, event: &AgentEvent) {
        let mut association = self.association.lock().unwrap();
        match &event.kind {
            EventKind::ContextPrepare(data) => {
                if association
                    .prepared
                    .as_ref()
                    .is_some_and(|prepared| prepared.request_id != data.request_id)
                {
                    association.prepared = None;
                }
            }
            EventKind::ModelCall(data) => {
                association.call = None;
                let Some(prepared) = association.prepared.take().filter(|prepared| {
                    prepared.request_id == data.request_id
                        && prepared.configuration == data.configuration
                        && prepared.prompt == data.prompt
                        && prepared
                            .request
                            .upgrade()
                            .is_some_and(|request| Arc::ptr_eq(&request, &data.request))
                }) else {
                    return;
                };
                association.call = Some(Arc::new(CallSource {
                    request_id: data.request_id.clone(),
                    source: TimelinePosition {
                        branch_id: self.timeline.branch_id(),
                        sequence: prepared.through,
                    },
                    anchor: TimelinePosition {
                        branch_id: self.timeline.branch_id(),
                        sequence: event.sequence,
                    },
                    calibration: prepared.calibration,
                }));
            }
            EventKind::ModelUsage(data) => {
                if let Some(call) = association.call.clone()
                    && call.request_id == data.request_id
                {
                    association.measurements.insert(event.sequence, call);
                }
            }
            EventKind::ContextConfigured(_)
            | EventKind::ContextCompaction(ContextCompactionData {
                outcome: ContextCompactionResult::Completed { .. },
                ..
            }) => {
                association.prepared = None;
                association.call = None;
            }
            _ => (),
        }
    }

    pub(crate) async fn reference(
        &self,
        source: &ContextPrepareData,
        through: u64,
        request_value: &Arc<ModelRequest>,
        compaction: Option<&str>,
        cancel: CancellationToken,
    ) -> std::result::Result<Option<UsageEvidence>, HostError> {
        let source_through = source.through;
        if through > self.timeline.sequence() && through <= self.timeline.committed() {
            tokio::select! {result=self.publication(through)=>result.map_err(|error|HostError::failed(error.to_string()))?,_=cancel.cancelled()=>return Err(HostError::Cancelled("Usage proof cancelled".into()))}
        }
        tokio::select! {
            result = self.delivery(through) => result.map_err(|error| HostError::failed(error.to_string()))?,
            _ = cancel.cancelled() => return Err(HostError::Cancelled("Usage proof cancelled".into())),
        }
        if through > source_through {
            let changed = self
                .timeline
                .query(TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: Some(TimelinePosition {
                        branch_id: self.timeline.branch_id(),
                        sequence: source_through + 1,
                    }),
                    through: Some(through),
                    include_publishing: false,
                    query: QuerySpec {
                        filter: Some(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "kind".into(),
                                    },
                                    values: [
                                        "input/consumed",
                                        "step/model-output-end",
                                        "step/tool-result",
                                        "context/configured",
                                        "context/compaction",
                                    ]
                                    .into_iter()
                                    .map(Into::into)
                                    .collect(),
                                },
                                QueryFilter::Or {
                                    conditions: vec![
                                        QueryFilter::In {
                                            value: QueryOperand::Field {
                                                path: "context.render".into(),
                                            },
                                            values: vec![true.into()],
                                        },
                                        QueryFilter::In {
                                            value: QueryOperand::Field {
                                                path: "kind".into(),
                                            },
                                            values: vec!["context/configured".into()],
                                        },
                                    ],
                                },
                                QueryFilter::Not {
                                    condition: Box::new(QueryFilter::In {
                                        value: QueryOperand::Field { path: "id".into() },
                                        values: compaction.into_iter().map(Into::into).collect(),
                                    }),
                                },
                            ],
                        }),
                        select: QuerySelect::Exists,
                        group_by: vec![],
                        pick: None,
                        order_by: vec![],
                    },
                    page: None,
                })
                .await
                .map_err(|error| HostError::failed(error.to_string()))?;
            let QueryOutput::Exists { exists } = changed.result else {
                unreachable!("request prefix changes")
            };
            if exists {
                return Ok(None);
            }
        }

        let view = match self.at(through, cancel.clone()).await {
            Ok(view) => view,
            Err(
                ContextOpError::Model(_) | ContextOpError::NotConfigured | ContextOpError::NotReady,
            ) => return Ok(None),
            Err(ContextOpError::Read(error)) => return Err(HostError::failed(error.to_string())),
        };
        let state = view.state.expect("ready usage state");
        if state.at(through) < state.initialized {
            return Ok(None);
        }
        let numeric = Numeric::new(
            self.timeline.clone(),
            self.timeline.sequence(),
            state.epoch.clone(),
            state.legacy_result.clone(),
            state.initialized,
        );
        let summary = numeric
            .prefix(state.current.as_ref(), state.at(through), true)
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        let Some(summary) = summary else {
            return Ok(None);
        };
        let Some(Configuration::Ready { counter }) = &summary.configuration else {
            return Ok(None);
        };
        if compaction.is_some_and(|id| {
            summary
                .compression
                .as_ref()
                .is_none_or(|current| current.event_id != id)
        }) {
            return Ok(None);
        }
        let Some(active_count) = summary
            .message_count
            .and_then(|count| {
                count.checked_sub(
                    summary
                        .compression
                        .as_ref()
                        .map_or(Some(0), |compression| compression.removed_messages)?,
                )
            })
            .and_then(|count| {
                count.checked_add(
                    summary
                        .compression
                        .as_ref()
                        .map_or(Some(0), |compression| compression.replacement_messages)?,
                )
            })
        else {
            return Ok(None);
        };
        if active_count != request_value.messages.len() as u64 {
            return Ok(None);
        }
        let identity = request_identity(request_value, &counter.algorithm)
            .map_err(|error| HostError::failed(error.to_string()))?;
        let request_local = if identity == counter.request_identity {
            None
        } else {
            let actual_control = control_identity(request_value, &counter.algorithm)
                .map_err(|error| HostError::failed(error.to_string()))?;
            if counter.control_identity.as_deref() != Some(actual_control.as_str()) {
                return Ok(None);
            }
            let model = view
                .model
                .as_ref()
                .ok_or_else(|| HostError::failed("Missing context counter"))?;
            let Some(actual) = model
                .contribution_counter()
                .filter(|actual| actual.algorithm() == counter.algorithm)
            else {
                return Ok(None);
            };
            let cached = self
                .dynamic_fixed
                .lock()
                .unwrap()
                .as_ref()
                .filter(|cached| {
                    cached.configuration == counter.event_id && cached.identity == identity
                })
                .map(|cached| cached.fixed);
            let fixed = cached.unwrap_or_else(|| {
                actual.fixed(
                    request_value.system.as_deref(),
                    request_value.tools.as_deref().map(Vec::as_slice),
                )
            });
            let contribution = total(&summary)
                .and_then(|total| subtract(total, counter.fixed))
                .and_then(|messages| add(messages, fixed))
                .map_err(|error| HostError::failed(error.to_string()))?;
            Some(actual.estimate(contribution))
        };
        let (estimate, _) = estimation(
            &summary,
            request_local.map(|local| (local, identity.as_str())),
        )
        .map_err(|error| HostError::failed(error.to_string()))?;
        if total(&summary)
            .map_err(|error| HostError::failed(error.to_string()))?
            .images
            == 0
        {
            self.association.lock().unwrap().prepared = Some(PreparedSource {
                request_id: source.request_id.clone(),
                through,
                request: Arc::downgrade(request_value),
                configuration: source.configuration.clone(),
                prompt: source.prompt.clone(),
                calibration: RequestCalibration {
                    configuration: counter
                        .identity()
                        .map_err(|error| HostError::failed(error.to_string()))?,
                    compression: summary
                        .compression
                        .as_ref()
                        .map(|compression| compression.event_id.clone()),
                    request_identity: request_local.map(|_| identity),
                    algorithm: counter.algorithm.clone(),
                    local: request_local
                        .or(summary.local)
                        .ok_or_else(|| HostError::failed("Missing request estimate"))?,
                },
            });
        }
        Ok(Some(UsageEvidence {
            boundary: summary.last_entry.clone(),
            estimate,
            limits: counter.limits.clone(),
            base: sources::RequestBase {
                epoch: state.epoch.clone(),
                source: TimelinePosition {
                    branch_id: self.timeline.branch_id(),
                    sequence: through,
                },
                algorithm: counter.algorithm.clone(),
                current: state.current.clone(),
                legacy_result: state.legacy_result.clone(),
                initialized: state.initialized,
            },
        }))
    }

    pub(crate) async fn growth(
        &self,
        evidence: &UsageEvidence,
        after: &TimelinePosition,
    ) -> std::result::Result<Option<f64>, HostError> {
        let segments = self
            .timeline
            .segments(self.timeline.sequence())
            .map_err(|error| HostError::failed(error.to_string()))?;
        let key = |position: &TimelinePosition| -> std::result::Result<Key, HostError> {
            let branch = segments
                .iter()
                .position(|(branch, _)| *branch == position.branch_id)
                .ok_or_else(|| HostError::failed("Growth boundary is outside the branch chain"))?;
            Ok(Key {
                branch: branch as u32,
                sequence: position.sequence,
            })
        };
        Numeric::new(
            self.timeline.clone(),
            self.timeline.sequence(),
            evidence.base.epoch.clone(),
            evidence.base.legacy_result.clone(),
            evidence.base.initialized,
        )
        .growth(
            evidence.base.current.as_ref(),
            key(after)?,
            key(&evidence.base.source)?,
        )
        .await
        .map_err(|error| HostError::failed(error.to_string()))
    }

    pub(crate) async fn read(
        &self,
        through: Option<u64>,
        cancel: CancellationToken,
    ) -> std::result::Result<UsageRead, ContextOpError> {
        let through = match through {
            Some(through) => through,
            None => {
                self.at(0, cancel.clone()).await?;
                self.timeline.sequence()
            }
        };
        let view = self.at(through, cancel).await?;
        let state = view.state.expect("ready usage state");
        let end = state.at(through);
        if end < state.initialized {
            return Err(ContextOpError::Model(HostError::failed(
                "Usage statistics were initialized after this request boundary",
            )));
        }
        let numeric = Numeric::new(
            self.timeline.clone(),
            self.timeline.sequence(),
            state.epoch.clone(),
            state.legacy_result.clone(),
            state.initialized,
        );
        let summary = numeric
            .prefix(state.current.as_ref(), end, true)
            .await
            .map_err(ContextOpError::Read)?
            .ok_or_else(|| ContextOpError::Read(fault("missing initialized usage total")))?;
        if let Some(Configuration::Ready { counter }) = &summary.configuration
            && counter.configured_tools != Some(true)
        {
            let model = view
                .model
                .as_ref()
                .ok_or_else(|| ContextOpError::Read(fault("missing context counter")))?;
            let fixed = self
                .current_tools(counter, model.as_ref(), through)
                .await
                .map_err(ContextOpError::Read)?;
            let counter_model = model
                .contribution_counter()
                .ok_or_else(|| ContextOpError::Read(fault("missing additive counter")))?;
            let local = counter_model.estimate(
                add(
                    subtract(
                        total(&summary).map_err(ContextOpError::Read)?,
                        counter.fixed,
                    )
                    .map_err(ContextOpError::Read)?,
                    fixed.fixed,
                )
                .map_err(ContextOpError::Read)?,
            );
            return reading(through, &summary, Some((local, &fixed.identity)));
        }
        reading(through, &summary, None)
    }

    async fn current_tools(
        &self,
        configuration: &CounterConfiguration,
        model: &dyn ContextModel,
        through: u64,
    ) -> Result<DynamicFixed> {
        if configuration.configured_tools.is_none()
            && self
                .configured_tools(&configuration.event_id, through)
                .await?
        {
            return Ok(DynamicFixed {
                configuration: configuration.event_id.clone(),
                versions: String::new(),
                identity: configuration.request_identity.clone(),
                fixed: configuration.fixed,
            });
        }
        let sources = self.registry.sources();
        let versions = fingerprint(
            &sources
                .iter()
                .map(|source| (&source.key, &source.version))
                .collect::<Vec<_>>(),
        )?;
        if let Some(cached) = self
            .dynamic_fixed
            .lock()
            .unwrap()
            .as_ref()
            .filter(|cached| {
                cached.configuration == configuration.event_id && cached.versions == versions
            })
        {
            return Ok(cached.clone());
        }
        let (tools, sources) = self.registry.snapshot();
        let versions = fingerprint(
            &sources
                .iter()
                .map(|source| (&source.key, &source.version))
                .collect::<Vec<_>>(),
        )?;
        let mut system = self
            .fields(&configuration.event_id, through, &["data.system"])
            .await?;
        let system: Option<SystemPrompt> = sources::optional(system.remove(0))?;
        let identity = fingerprint(&(
            &configuration.control_identity,
            fingerprint(&(&system, &Some(&tools)))?,
        ))?;
        let previous = self
            .dynamic_fixed
            .lock()
            .unwrap()
            .as_ref()
            .filter(|cached| {
                cached.configuration == configuration.event_id && cached.identity == identity
            })
            .map(|cached| cached.fixed);
        let counter = model
            .contribution_counter()
            .filter(|counter| counter.algorithm() == configuration.algorithm)
            .ok_or_else(|| fault("context counter algorithm changed"))?;
        let fixed = previous.unwrap_or_else(|| counter.fixed(system.as_ref(), Some(&tools)));
        let value = DynamicFixed {
            configuration: configuration.event_id.clone(),
            versions,
            identity,
            fixed,
        };
        *self.dynamic_fixed.lock().unwrap() = Some(value.clone());
        Ok(value)
    }

    async fn at(
        &self,
        through: u64,
        cancel: CancellationToken,
    ) -> std::result::Result<Progress, ContextOpError> {
        if self.resolver.0.is_none() {
            return Err(ContextOpError::Model(HostError::failed(
                "Context compaction is not available",
            )));
        }
        if through > self.timeline.sequence() {
            return Err(ContextOpError::Read(AgentError::InvalidInput(
                "future usage boundary".into(),
            )));
        }
        self.handoff.fetch_max(through, Ordering::SeqCst);
        self.wanted.fetch_max(through, Ordering::SeqCst);
        self.wake.notify_one();
        let mut progress = self.progress.subscribe();
        loop {
            let view = progress.borrow_and_update().clone();
            if let Some(error) = &view.error {
                return Err(ContextOpError::Read(error.clone()));
            }
            if view.state.is_some()
                && view.covered >= through
                && (view.state.as_ref().is_none_or(|state| {
                    state
                        .current
                        .as_ref()
                        .is_none_or(|current| current.totals.configuration.is_none())
                }) || view.needs_history
                    || view.relevant_through > through
                    || view.saved_through >= view.relevant_through)
            {
                if let Some(error) = &view.unavailable {
                    return Err(ContextOpError::Model(error.clone()));
                }
                if view.needs_history {
                    return Err(ContextOpError::NotReady);
                }
                return Ok(view);
            }
            tokio::select! {
                changed = progress.changed() => changed.map_err(|_| ContextOpError::Read(AgentError::Closed("ContextUsage".into())))?,
                _ = cancel.cancelled() => return Err(ContextOpError::Model(HostError::Cancelled("Usage read cancelled".into()))),
                _ = self.cancel.cancelled() => return Err(ContextOpError::Read(AgentError::Closed("ContextUsage".into()))),
                _ = self.branch.closing() => return Err(ContextOpError::Read(AgentError::Closed("ContextUsage".into()))),
            }
        }
    }

    async fn delivery(&self, sequence: u64) -> Result<()> {
        let mut delivered = self.dispatcher.watch();
        loop {
            let progress = delivered.borrow_and_update().clone();
            if let Some(error) = progress.failure {
                return Err(error);
            }
            if progress.sequence >= sequence {
                return Ok(());
            }
            tokio::select! {
                changed = delivered.changed() => changed.map_err(|_| fault("usage delivery closed"))?,
                _ = self.cancel.cancelled() => return Err(AgentError::Closed("ContextUsage".into())),
                _ = self.branch.closing() => return Err(AgentError::Closed("ContextUsage".into())),
            }
        }
    }

    async fn publication(&self, sequence: u64) -> Result<()> {
        let mut published = self.timeline.watch();
        while published.borrow_and_update().0 < sequence {
            if let Some(error) = self.timeline.failure() {
                return Err(error);
            }
            tokio::select! {
                changed = published.changed() => changed.map_err(|_| fault("publication channel closed"))?,
                _ = self.cancel.cancelled() => return Err(AgentError::Closed("ContextUsage".into())),
                _ = self.branch.closing() => return Err(AgentError::Closed("ContextUsage".into())),
            }
        }
        Ok(())
    }

    async fn save(&self, machine: &mut Machine) -> Result<()> {
        let pending = machine.numeric.take_pending();
        if pending.is_empty() {
            return Ok(());
        }
        let count = pending.len();
        let mut drafts = Vec::with_capacity(count);
        for (index, reference) in pending.into_iter().enumerate() {
            let at = reference.at;
            let mut state = machine.state.clone();
            state.current = Some(reference);
            let source_through = if index + 1 == count {
                machine.source_through
            } else if at.branch == state.branches.len() as u32 - 1 {
                at.sequence
            } else {
                0
            };
            drafts.push(EventDraft::new(EventKind::Extension(
                ExtensionEvent::new(
                    "context-usage",
                    "recovered",
                    serde_json::to_value(Recovered {
                        schema: SCHEMA.into(),
                        source_through,
                        state,
                    })
                    .map_err(|error| fault(error.to_string()))?,
                )
                .expect("usage numeric key"),
            )));
        }
        let written = self.timeline.append_batch_registered(drafts, |_| Ok(()))?;
        let last = written.last().expect("numeric result");
        self.publication(last.sequence).await?;
        machine.result = last.sequence;
        machine.result_id = Some(last.id.clone());
        machine.saved_through = machine.source_through;
        machine.numeric.published(machine.result);
        Ok(())
    }

    async fn cover(&self, machine: &mut Machine) -> Result<()> {
        if machine.needs_history || machine.relevant_through <= machine.saved_through {
            return Ok(());
        }
        let Some(result_id) = &machine.result_id else {
            return Ok(());
        };
        let event = self.timeline.append(
            EventKind::Extension(
                ExtensionEvent::new(
                    "context-usage",
                    "covered",
                    serde_json::to_value(Covered {
                        schema: "context-usage-covered/v1".into(),
                        result_id: result_id.clone(),
                        source_through: machine.source_through,
                    })
                    .map_err(|error| fault(error.to_string()))?,
                )
                .expect("usage covered key"),
            )
            .into(),
        )?;
        self.publication(event.sequence).await?;
        machine.saved_through = machine.source_through;
        Ok(())
    }

    fn publish(&self, machine: &Machine, covered: u64) {
        self.progress.send_replace(Progress {
            state: Some(Arc::new(machine.state.clone())),
            covered,
            relevant_through: machine.relevant_through,
            saved_through: machine.saved_through,
            unavailable: machine.unavailable.clone(),
            needs_history: machine.needs_history,
            model: machine.model.clone(),
            error: None,
        });
    }
}

pub(crate) struct ContextUsagePlugin {
    usage: ServiceKey<ContextUsage>,
    timeline: ServiceKey<Timeline>,
    resolver: ServiceKey<ContextModelHost>,
    registry: ServiceKey<ToolRegistry>,
    dispatcher: ServiceKey<Dispatcher>,
    branch: ServiceKey<BranchLifecycle>,
}

impl ContextUsagePlugin {
    pub(crate) fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            usage: names.key()?,
            timeline: names.key()?,
            resolver: names.key()?,
            registry: names.key()?,
            dispatcher: names.key()?,
            branch: names.key()?,
        })
    }
}

impl DynPlugin for ContextUsagePlugin {
    fn name(&self) -> &str {
        "ContextUsage"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.resolver.erase(),
            self.registry.erase(),
            self.dispatcher.erase(),
            self.branch.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let owner = Arc::new(ContextUsage {
                timeline: ctx.require_at(&self.timeline)?,
                resolver: ctx.require_at(&self.resolver)?,
                registry: ctx.require_at(&self.registry)?,
                dynamic_fixed: Mutex::new(None),
                dispatcher: ctx.require_at(&self.dispatcher)?,
                branch: ctx.require_at(&self.branch)?,
                wanted: AtomicU64::new(0),
                handoff: AtomicU64::new(0),
                wake: Notify::new(),
                progress: watch::channel(Progress::default()).0,
                tasks: TaskTracker::new(),
                cancel: ctx.cancellation_token(),
                errors: Mutex::new(Vec::new()),
                association: Mutex::new(Association::default()),
                initial_history: Mutex::new(None),
            });
            let cleanup = owner.clone();
            ctx.effect(move || async move {
                cleanup.cancel.cancel();
                cleanup.tasks.close();
                cleanup.tasks.wait().await;
                AgentError::aggregate(cleanup.errors.lock().unwrap().clone()).map_err(Into::into)
            })?;
            if owner.resolver.0.is_some() {
                let initial = owner.timeline.sequence();
                let notify = owner.clone();
                owner.dispatcher.subscribe(
                    ctx,
                    "ContextUsage",
                    &[
                        "context/configured",
                        "input/consumed",
                        "step/model-output-end",
                        "step/tool-result",
                        "context/compaction",
                        "model/usage",
                        "context/prepare",
                        "model/call",
                        "turn/end",
                    ],
                    move |event| {
                        let owner = notify.clone();
                        Box::pin(async move {
                            owner.associate(&event);
                            if matches!(event.kind, EventKind::TurnEnd(_)) {
                                owner.handoff.fetch_max(event.sequence, Ordering::SeqCst);
                            }
                            owner.wanted.fetch_max(event.sequence, Ordering::SeqCst);
                            owner.wake.notify_one();
                            Ok(())
                        })
                    },
                )?;
                let task = owner.clone();
                owner.tasks.spawn(async move {
                    let started = tokio::select! {
                        started = task.branch.activated() => started,
                        _ = task.cancel.cancelled() => false,
                    };
                    if !started {
                        return;
                    }
                    let result = tokio::select! {
                        result = AssertUnwindSafe(task.run(initial)).catch_unwind() => result
                            .map_err(|panic| fault(crate::host::panic_message(panic)))
                            .and_then(|result| result),
                        _ = task.cancel.cancelled() => return,
                        _ = task.branch.closing() => return,
                    };
                    if let Err(error) = result {
                        if matches!(error, AgentError::Closed(_))
                            && (task.cancel.is_cancelled()
                                || task.branch.phase() >= dim_agent_runtime::BranchPhase::Closing)
                        {
                            return;
                        }
                        task.errors.lock().unwrap().push(error.clone());
                        task.progress
                            .send_modify(|progress| progress.error = Some(error.clone()));
                        task.dispatcher.fail(error);
                    }
                });
            }
            ctx.provide_at(self.usage.clone(), owner)?;
            Ok(Effect::Done)
        })
    }
}

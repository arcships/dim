use crate::{
    history::{
        HistoryEntry, HistoryRenderer, chain_by_id, context_compaction, context_configuration,
        digest_message, newest_chain, rising_chain, truncate_middle, user_message,
    },
    host::*,
};
use dim_agent_runtime::{
    ContextHistory, PredicateOp, Timeline, TimelineQuery, TimelineQueryDirection,
    TimelineQueryOrderBy, TimelineQueryPredicate, TimelineQueryScope,
};
use dim_agent_types::*;
use serde_json::json;
use std::{
    collections::HashMap,
    num::NonZeroU64,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

/// Share of the input budget a compaction keeps verbatim, the keyed baseline
/// included; provisional (#128, #428).
const RETAINED_SHARE: f64 = 0.25;

/// Share of the input budget the running turn's pinned inputs may take
/// (ContextCompaction §5.9); provisional (#428).
const PINNED_SHARE: f64 = 0.1;

/// Share of the input budget the whole context after a compaction is held
/// to at most (ContextCompaction §5.3); provisional (#428).
const BOUND_SHARE: f64 = 0.5;

/// Share of the input budget the digest part may take (ContextCompaction
/// §5.10); provisional (#428).
const DIGEST_SHARE: f64 = 0.05;

/// Facts one page of the compaction-reuse read keeps.
const REUSE_PAGE: u64 = 64;

/// Turn starts one page of a cut order keeps.
const CUT_PAGE: u64 = 64;

/// Events one page of a cut window keeps.
const WINDOW_PAGE: u64 = 256;

pub(crate) struct ContextPolicy {
    pub timeline: Arc<Timeline>,
    pub resolver: Arc<dyn ResolveContextModel>,
    pub strategy: Arc<dyn CompactionStrategy>,
    pub history: Arc<HistoryRenderer>,
    /// The latest prepared request and its estimate, so `validate` checks
    /// it without reading the context again; derived, a miss reads it.
    prepared: Mutex<Option<(Arc<ModelRequest>, f64)>>,
}
pub(crate) struct Estimate {
    pub input: f64,
    pub source: BaselineSource,
    baseline: Option<f64>,
    delta: Option<f64>,
}

/// The caller of a compaction step and its reason; the ids and `through`
/// are written into both facts.
pub(crate) struct CompactionStep {
    pub turn_id: Option<String>,
    pub request_id: Option<String>,
    pub command: Option<u64>,
    pub through: u64,
    pub trigger: CompactionTrigger,
    pub provider_budget: Option<ProviderOutputBudgetFacts>,
}

/// A request's context at a fixed boundary, projected under the compaction
/// in effect for it.
/// The context a policy preparation consumes. The full history is retained
/// only for compaction snapshots; streamed preparations omit it.
pub(crate) struct PolicyContext {
    configuration: Option<ContextConfiguredData>,
    compaction: Option<ContextCompactionData>,
    full: Option<ContextHistory>,
    /// Root first: how positions compare across branches.
    branches: Vec<String>,
    /// The position of the in-effect cut, when the stream resolved it.
    compaction_cut: Option<TimelinePosition>,
}

impl PolicyContext {
    pub fn from_context(context: ContextHistory) -> Self {
        let configuration = context_configuration(&context);
        let compaction = context_compaction(&context).cloned();
        let mut branches: Vec<String> = Vec::new();
        for position in &context.positions {
            if !branches.contains(&position.branch_id) {
                branches.push(position.branch_id.clone());
            }
        }
        Self {
            configuration,
            compaction,
            full: Some(context),
            branches,
            compaction_cut: None,
        }
    }

    pub fn from_stream(streamed: &crate::history::StreamedContext) -> Self {
        let compaction = streamed
            .compaction
            .as_ref()
            .and_then(|event| match &event.kind {
                EventKind::ContextCompaction(data) => Some(data.clone()),
                _ => None,
            });
        Self {
            configuration: streamed.configuration.clone(),
            compaction,
            full: None,
            branches: streamed.branches.clone(),
            compaction_cut: streamed.compaction_cut.clone(),
        }
    }

    pub fn configuration(&self) -> Option<&ContextConfiguredData> {
        self.configuration.as_ref()
    }

    pub fn compaction(&self) -> Option<&ContextCompactionData> {
        self.compaction.as_ref()
    }

    pub fn messages_len(&self, history: &[HistoryEntry]) -> usize {
        crate::history::projected_messages_len(self.compaction(), history)
    }

    pub fn messages(&self, history: &[HistoryEntry]) -> Vec<Arc<Message>> {
        crate::history::projected_messages(self.compaction(), history)
    }

    /// The same messages by reference, for checks that only compare them.
    pub fn message_refs<'a>(
        &'a self,
        history: &'a [HistoryEntry],
    ) -> impl Iterator<Item = &'a Message> + 'a {
        crate::history::projected_message_refs(self.compaction(), history)
    }

    /// The index of the first entry at or after a chain position.
    pub fn position_entry(&self, history: &[HistoryEntry], position: &TimelinePosition) -> usize {
        let rank = |branch: &str| self.branches.iter().position(|name| name == branch);
        let at = (rank(&position.branch_id), position.sequence);
        history
            .iter()
            .position(|entry| (rank(&entry.position.branch_id), entry.position.sequence) >= at)
            .unwrap_or(history.len())
    }

    /// The index of the first entry a cut keeps, when the context places the
    /// cut itself: it is an entry, or the compaction in effect's cut.
    pub fn retained_entry(&self, history: &[HistoryEntry], cut: &str) -> Option<usize> {
        if let Some(index) = history.iter().position(|entry| entry.event_id == cut) {
            return Some(index);
        }
        let position = self.compaction_cut.as_ref().filter(|_| {
            self.compaction.as_ref().is_some_and(|data| {
                matches!(&data.outcome,
                    ContextCompactionResult::Completed { retain_from, .. } if retain_from == cut)
            })
        })?;
        Some(self.position_entry(history, position))
    }

    pub fn full(&self) -> Option<&ContextHistory> {
        self.full.as_ref()
    }
}

/// One fact a request's estimate compares against, with where it sits.
pub(crate) type EstimateEvent = (TimelinePosition, Arc<AgentEvent>);

/// One event a compaction step may cut at, with where it sits.
struct OrderEvent {
    id: String,
    position: TimelinePosition,
    turn_start: bool,
    /// The branch's last event at its bound: the cut that replaces everything.
    boundary: bool,
}

/// The events a compaction step chooses its cuts from: every turn start after
/// the retention rule's window and the branch's last event, in chain order.
struct CutOrder {
    /// Root first: how positions compare across branches.
    branches: Vec<String>,
    events: Vec<OrderEvent>,
}

/// The events a chosen cut needs: where the cut sits, the turn's input window
/// before it, and the tool results of the compaction in effect after it.
struct CutWindow {
    cut_position: Option<TimelinePosition>,
    events: Vec<Arc<AgentEvent>>,
    tool_results: Vec<(TimelinePosition, Arc<AgentEvent>)>,
}

pub(crate) struct View {
    /// The request as built from the Timeline, with the history prefix.
    request: Arc<ModelRequest>,
    prefix_len: usize,
    context: PolicyContext,
    /// The entries from the cut in effect on.
    history: Vec<HistoryEntry>,
    /// The sequence the preparation reads to.
    through: u64,
    /// The events an estimate compares against, oldest first, and the ones a
    /// retry may reuse.
    events: Vec<Arc<AgentEvent>>,
    checkpoint: Option<CompactionCheckpoint>,
    /// `request` projected under `checkpoint`.
    pub prepared: Arc<ModelRequest>,
}

/// The committed result of a step and, when it completed, the request
/// projected on it, or why the committed result cannot be adopted.
pub(crate) struct Compacted {
    pub fact: Arc<AgentEvent>,
    pub request: Option<Result<Arc<ModelRequest>, HostError>>,
}

/// A completed strategy result composed on the view: its cut, the running
/// turn's pins, the projected request and the tokens it counts.
struct Composed {
    retain_from: String,
    replacement: Vec<Message>,
    parts: Option<ReplacementParts>,
    /// The digest's entries as data, when the replacement carries a digest
    /// message (§5.10).
    digest: Option<DigestEntries>,
    /// The `input/message` id of each pinned message, in order (§5.9).
    ids: Option<Vec<String>>,
    baseline: Arc<[ContextBaselineEntry]>,
    /// The index in the view's history of the first kept entry.
    kept: usize,
    tokens: f64,
}

/// What one strategy pass produced, composed or not.
enum StepOutcome {
    Completed {
        composed: Composed,
        diagnostics: StrategyDiagnostics,
    },
    Skipped {
        reason: String,
        diagnostics: StrategyDiagnostics,
    },
    Failed {
        retain_from: Option<String>,
        estimated_output_tokens: Option<f64>,
        error: String,
        diagnostics: Option<StrategyDiagnostics>,
    },
    Cancelled {
        reason: String,
    },
}

/// One pass of a compaction: the view it composes on, the turn it pins, and
/// the entries and cuts it may keep. A re-plan passes the entries from the
/// first result's cut on and starts its kept entries at that result's.
struct Pass<'a> {
    view: &'a View,
    model: &'a dyn ContextModel,
    /// The running turn and the pins it already carries (§5.9).
    turn: Option<(&'a str, Option<&'a CarriedPins>)>,
    recovery: bool,
    history: &'a [HistoryEntry],
    /// The index in the view's history where `history` starts.
    offset: usize,
    cuts: &'a [CompactionCut],
}

impl Pass<'_> {
    fn compose(
        &self,
        window: &CutWindow,
        retain_from: String,
        strategy_messages: Vec<Message>,
    ) -> Result<Composed, &'static str> {
        let within = self.cuts.iter().any(|cut| cut.within_limit);
        let cut = self
            .cuts
            .iter()
            .find(|cut| cut.retain_from == retain_from)
            .ok_or_else(|| {
                if window.events.iter().any(|event| event.id == retain_from) {
                    "Compaction cut separates a tool call from its results or context."
                } else {
                    "Compaction cut is outside the fixed context history."
                }
            })?;
        if !(cut.within_limit || (!within && (self.recovery || cut.entry < self.history.len()))) {
            return Err("Compaction cut lies inside the retained tail.");
        }
        let pinned = pinned_inputs(self.view, Some(window), self.turn, &retain_from, self.model);
        let digest = digest_entries(self.view, window, self.model);
        let message = digest
            .as_ref()
            .map(|digest| digest_message(&digest.entries, digest.dropped));
        let parts =
            (!pinned.messages.is_empty() || message.is_some()).then_some(ReplacementParts {
                strategy: strategy_messages.len() as u64,
                digest: message.is_some() as u64,
            });
        let mut replacement = strategy_messages;
        replacement.extend(message);
        replacement.extend(pinned.messages);
        let kept = self.offset + cut.entry;
        let baseline = self.view.history[kept - 1].baseline.clone();
        let tokens = self
            .view
            .compacted(self.model, kept, &replacement, &baseline)
            .1;
        Ok(Composed {
            retain_from,
            replacement,
            parts,
            digest,
            ids: pinned.ids,
            baseline,
            kept,
            tokens,
        })
    }
}

fn checkpoint(data: &ContextCompactionData) -> Option<CompactionCheckpoint> {
    match &data.outcome {
        ContextCompactionResult::Completed {
            retain_from,
            replacement,
            context_baseline,
            replacement_parts,
            ..
        } => Some(CompactionCheckpoint {
            retain_from: retain_from.clone(),
            replacement: replacement.clone().into(),
            context_baseline: context_baseline.clone().into(),
            replacement_parts: *replacement_parts,
        }),
        _ => None,
    }
}

/// The request a preparation at this boundary would start from: the latest
/// adopted configuration and system with the projected messages. Tools are
/// declared but not bound; nothing executes them.
pub(crate) fn configured_request(
    context: &PolicyContext,
    history: &[HistoryEntry],
) -> Option<Arc<ModelRequest>> {
    let data = context.configuration()?;
    Some(Arc::new(
        ModelSettings {
            provider_id: data.settings.provider_id.clone(),
            model_id: data.settings.model_id.clone(),
            capability_snapshot: data.settings.capability_snapshot.clone(),
            tools: data.settings.tools.clone(),
            system: data.system.clone(),
            config: crate::context_model::adopted_config(
                data.settings.capability_snapshot.as_ref(),
                data.settings.config.as_ref(),
            ),
            metadata: data.settings.metadata.clone(),
        }
        .request(context.messages(history)),
    ))
}

impl ContextPolicy {
    /// The policy exists when both hosts are bound; one without the other
    /// is an assembly defect.
    pub fn bind(
        timeline: &Arc<Timeline>,
        model: &ContextModelHost,
        strategy: &CompactionStrategyHost,
        history: &Arc<HistoryRenderer>,
    ) -> Result<Option<Self>, String> {
        match (&model.0, &strategy.0) {
            (Some(resolver), Some(strategy)) => Ok(Some(Self {
                timeline: timeline.clone(),
                resolver: resolver.clone(),
                strategy: strategy.clone(),
                history: history.clone(),
                prepared: Mutex::new(None),
            })),
            (None, None) => Ok(None),
            _ => Err("ContextModelHost and CompactionStrategyHost must be bound together".into()),
        }
    }

    /// Projects `request` under the compaction in effect: the context's own,
    /// or a later one of the same source that a retry reuses (`reusable`).
    /// A reused cut the context's order does not carry is placed by one
    /// by-id read: the order holds the anchors, not every turn start.
    pub async fn view(
        &self,
        request: Arc<ModelRequest>,
        context: PolicyContext,
        history: Vec<HistoryEntry>,
        through: u64,
        events: Vec<Arc<AgentEvent>>,
        reusable: Vec<Arc<AgentEvent>>,
    ) -> Result<View, HostError> {
        let prefix_len = context.messages_len(&history);
        let mut reused = None;
        for event in reusable.iter().rev() {
            let EventKind::ContextCompaction(data) = &event.kind else {
                continue;
            };
            let Some(checkpoint) = checkpoint(data) else {
                continue;
            };
            let kept = match context.retained_entry(&history, &checkpoint.retain_from) {
                Some(kept) => kept,
                None => {
                    let Some(position) = self.cut_position(&checkpoint.retain_from).await? else {
                        continue;
                    };
                    context.position_entry(&history, &position)
                }
            };
            reused = Some((checkpoint, kept));
            break;
        }
        let (history, checkpoint, prepared) = match reused {
            Some((checkpoint, kept)) => {
                let history = history[kept..].to_vec();
                let prepared = project(
                    &request,
                    prefix_len,
                    &checkpoint.replacement,
                    &checkpoint.context_baseline,
                    &history,
                );
                (history, Some(checkpoint), prepared)
            }
            None => (
                history,
                context.compaction().and_then(checkpoint),
                request.clone(),
            ),
        };
        let mut events = events;
        events.extend(reusable);
        Ok(View {
            request,
            prefix_len,
            context,
            history,
            through,
            events,
            checkpoint,
            prepared,
        })
    }

    /// Where the event `cut` sits in the branch chain, read by its id.
    async fn cut_position(&self, cut: &str) -> Result<Option<TimelinePosition>, HostError> {
        self.timeline
            .query(chain_by_id(cut))
            .await
            .map(|reply| reply.positions.into_iter().next())
            .map_err(|error| HostError::failed(error.to_string()))
    }

    /// The index of the first entry a cut keeps, resolving a cut the history
    /// and the compaction in effect do not place with one by-id read.
    async fn kept_entry(&self, view: &View, cut: &str) -> Result<usize, HostError> {
        if let Some(kept) = view.context.retained_entry(&view.history, cut) {
            return Ok(kept);
        }
        let position = self.cut_position(cut).await?.ok_or_else(|| {
            HostError::failed(format!("Compaction cut {cut} is outside the branch chain"))
        })?;
        Ok(view.context.position_entry(&view.history, &position))
    }

    /// The events a compaction step chooses its cuts from: every turn start
    /// after the retention rule's window and the branch's last event at the
    /// preparation's bound, in chain order.
    async fn cut_order(
        &self,
        view: &View,
        model: &dyn ContextModel,
    ) -> Result<CutOrder, HostError> {
        let through = view.through;
        let branches: Vec<String> = self
            .timeline
            .segments(through)
            .map_err(|error| HostError::failed(error.to_string()))?
            .into_iter()
            .map(|(branch, _)| branch)
            .collect();
        let window = cut_window(&view.history, model);
        let mut events = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .timeline
                .query(rising_chain(
                    window.map(|entry| entry.event_id.as_str()),
                    vec!["turn/start".into()],
                    offset,
                    CUT_PAGE,
                ))
                .await
                .map_err(|error| HostError::failed(error.to_string()))?;
            let short = (page.events.len() as u64) < CUT_PAGE;
            events.extend(
                page.events
                    .into_iter()
                    .zip(page.positions)
                    .map(|(event, position)| OrderEvent {
                        id: event.id.clone(),
                        position,
                        turn_start: true,
                        boundary: false,
                    }),
            );
            if short {
                break;
            }
            offset += CUT_PAGE;
        }
        let page = self
            .timeline
            .query(newest_chain(None, Some(through), None, 1))
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        if let Some((event, position)) = page.events.into_iter().zip(page.positions).next()
            && position.branch_id == self.timeline.branch_id()
        {
            events.push(OrderEvent {
                id: event.id.clone(),
                position,
                turn_start: matches!(&event.kind, EventKind::TurnStart(_)),
                boundary: true,
            });
        }
        let rank = |branch: &str| branches.iter().position(|name| name == branch);
        events.sort_by_key(|event| (rank(&event.position.branch_id), event.position.sequence));
        Ok(CutOrder { branches, events })
    }

    /// Whether at least `RETAINED_SHARE × B` of new history has accumulated
    /// since the branch's latest compaction attempt, of any trigger and any
    /// outcome (ContextCompaction §5.6).
    async fn regrown(&self, view: &View, model: &dyn ContextModel) -> Result<bool, HostError> {
        let page = self
            .timeline
            .query(newest_chain(
                Some("context/compaction-start"),
                Some(view.through),
                None,
                1,
            ))
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        let start =
            page.events
                .into_iter()
                .zip(page.positions)
                .next()
                .and_then(|(event, position)| match &event.kind {
                    EventKind::ContextCompactionStart(data) => Some((position, data.through)),
                    _ => None,
                });
        Ok(regrown(
            view,
            start.as_ref().map(|(at, through)| (at, *through)),
            model,
        ))
    }

    /// The events a chosen cut needs: where the cut sits, the running turn's
    /// inputs before it, and the tool results since the compaction in effect's
    /// cut. One by-id read for the cut, one page for that compaction, then a
    /// page range for the turn's window and one for the tool results.
    async fn cut_window(
        &self,
        view: &View,
        turn_id: &str,
        cut: &str,
    ) -> Result<CutWindow, HostError> {
        let branches = view.context.branches.clone();
        let reply = self
            .timeline
            .query(chain_by_id(cut))
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        let cut_position = reply.positions.first().cloned();
        let mut events: Vec<(TimelinePosition, Arc<AgentEvent>)> =
            reply.positions.into_iter().zip(reply.events).collect();
        let previous = self
            .timeline
            .query(newest_chain(
                Some("context/compaction"),
                Some(view.through),
                Some(TimelineQueryPredicate {
                    op: PredicateOp::Eq,
                    path: "status".into(),
                    value: json!("completed"),
                }),
                1,
            ))
            .await
            .map_err(|error| HostError::failed(error.to_string()))?
            .events
            .into_iter()
            .next()
            .and_then(|event| match &event.kind {
                EventKind::ContextCompaction(data) => match &data.outcome {
                    ContextCompactionResult::Completed { retain_from, .. } => {
                        Some(retain_from.clone())
                    }
                    _ => None,
                },
                _ => None,
            });
        let previous = match &previous {
            Some(previous) => self
                .cut_position(previous)
                .await?
                .map(|position| (previous.clone(), position)),
            None => None,
        };
        let turn = if turn_id.is_empty() {
            None
        } else {
            let reply = self
                .timeline
                .query(newest_chain(
                    Some("turn/start"),
                    Some(view.through),
                    Some(TimelineQueryPredicate {
                        op: PredicateOp::Eq,
                        path: "turn_id".into(),
                        value: json!(turn_id),
                    }),
                    1,
                ))
                .await
                .map_err(|error| HostError::failed(error.to_string()))?;
            reply.events.into_iter().zip(reply.positions).next()
        };
        if let (Some((turn, _)), Some(at)) = (&turn, &cut_position) {
            let window = [
                "turn/start".to_owned(),
                "input/message".to_owned(),
                "input/consumed".to_owned(),
            ]
            .to_vec();
            let mut offset = 0;
            loop {
                let page = self
                    .timeline
                    .query(rising_chain(
                        Some(turn.id.as_str()),
                        window.clone(),
                        offset,
                        WINDOW_PAGE,
                    ))
                    .await
                    .map_err(|error| HostError::failed(error.to_string()))?;
                let short = (page.events.len() as u64) < WINDOW_PAGE;
                let mut passed = false;
                for (event, position) in page.events.into_iter().zip(page.positions) {
                    if later(&branches, &position, at) {
                        passed = true;
                        break;
                    }
                    events.push((position, event));
                }
                if short || passed {
                    break;
                }
                offset += WINDOW_PAGE;
            }
            self.supply_inputs(&mut events).await?;
        }
        let tools = previous.as_ref().map(|(cut, _)| cut.as_str());
        let mut tool_results = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .timeline
                .query(rising_chain(
                    tools,
                    vec!["step/tool-result".to_owned()],
                    offset,
                    WINDOW_PAGE,
                ))
                .await
                .map_err(|error| HostError::failed(error.to_string()))?;
            let short = (page.events.len() as u64) < WINDOW_PAGE;
            let mut passed = false;
            for (event, position) in page.events.into_iter().zip(page.positions) {
                if let Some(at) = &cut_position
                    && later(&branches, &position, at)
                {
                    passed = true;
                    break;
                }
                tool_results.push((position, event));
            }
            if short || passed {
                break;
            }
            offset += WINDOW_PAGE;
        }
        events.sort_by_key(|(position, _)| {
            let rank = branches
                .iter()
                .position(|branch| branch == &position.branch_id);
            (rank, position.sequence)
        });
        events.dedup_by(|(_, left), (_, right)| left.id == right.id);
        Ok(CutWindow {
            cut_position,
            events: events.into_iter().map(|(_, event)| event).collect(),
            tool_results,
        })
    }

    /// Adds the `input/message` of every input the window consumed that was
    /// written before the window started, so a pin names what the model saw.
    async fn supply_inputs(
        &self,
        events: &mut Vec<(TimelinePosition, Arc<AgentEvent>)>,
    ) -> Result<(), HostError> {
        let consumed: Vec<String> = events
            .iter()
            .filter_map(|(_, event)| match &event.kind {
                EventKind::InputConsumed(data) => Some(data.input_id.clone()),
                _ => None,
            })
            .collect();
        for id in consumed {
            if events.iter().any(|(_, event)| event.id == id) {
                continue;
            }
            let reply = self
                .timeline
                .query(chain_by_id(&id))
                .await
                .map_err(|error| HostError::failed(error.to_string()))?;
            events.extend(reply.positions.into_iter().zip(reply.events));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn prepare(
        &self,
        request: Arc<ModelRequest>,
        source: &ContextPrepareData,
        context: PolicyContext,
        history: Vec<HistoryEntry>,
        events: Vec<Arc<AgentEvent>>,
        cancel: CancellationToken,
        cancelled: &(dyn Fn() -> String + Sync),
    ) -> Result<Arc<ModelRequest>, HostError> {
        let (request, input) = self
            .prepare_estimated(request, source, context, history, events, cancel, cancelled)
            .await?;
        *self.prepared.lock().unwrap() = Some((request.clone(), input));
        Ok(request)
    }

    /// The prepared request and the trigger's estimate of it.
    #[allow(clippy::too_many_arguments)]
    async fn prepare_estimated(
        &self,
        request: Arc<ModelRequest>,
        source: &ContextPrepareData,
        context: PolicyContext,
        history: Vec<HistoryEntry>,
        events: Vec<Arc<AgentEvent>>,
        cancel: CancellationToken,
        cancelled: &(dyn Fn() -> String + Sync),
    ) -> Result<(Arc<ModelRequest>, f64), HostError> {
        let boundary = history.last().map(|entry| entry.id.clone());
        // An earlier attempt of this turn wrote these facts; they may still be
        // waiting for publication, so the read adds the branch's tail.
        let mut reusable = Vec::new();
        let mut from = source.through.saturating_add(1);
        loop {
            let reply = self
                .timeline
                .query(TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    include_publishing: true,
                    from_event: None,
                    from_sequence: Some(from),
                    order_by: TimelineQueryOrderBy::Sequence,
                    direction: TimelineQueryDirection::Ascending,
                    kinds: vec!["context/compaction".into(), "context/budget".into()],
                    flags: Vec::new(),
                    event_id: None,
                    predicate: Some(TimelineQueryPredicate {
                        op: PredicateOp::Eq,
                        path: "turn_id".into(),
                        value: json!(source.turn_id),
                    }),
                    predicates: Vec::new(),
                    text: None,
                    limit: NonZeroU64::new(REUSE_PAGE).expect("a page keeps at least one event"),
                    offset: 0,
                })
                .await
                .map_err(|e| HostError::failed(e.to_string()))?;
            let short = (reply.events.len() as u64) < REUSE_PAGE;
            let last = reply.events.last().map(|event| event.sequence);
            reusable.extend(reply.events.into_iter().filter(|event| match &event.kind {
                EventKind::ContextCompaction(data) => data.through == source.through,
                EventKind::ContextBudget(_) => true,
                _ => unreachable!("the read selects the two compaction facts"),
            }));
            match last {
                Some(last) if !short => from = last + 1,
                _ => break,
            }
        }
        let view = self
            .view(request, context, history, source.through, events, reusable)
            .await?;
        let model = self
            .resolver
            .resolve(view.prepared.clone(), cancel.clone())
            .await?;
        check_cancel(&cancel)?;
        let estimate = self.estimate(&view.prepared, model.as_ref(), &view.events)?;
        self.timeline
            .append(
                EventKind::ContextBudget(ContextBudgetData {
                    turn_id: source.turn_id.clone(),
                    request_id: source.request_id.clone(),
                    boundary,
                    estimated_input_tokens: estimate.input,
                    baseline_source: estimate.source,
                    baseline_prompt_tokens: estimate.baseline,
                    delta_tokens: estimate.delta,
                    context_window: model.context_window(),
                    output_tokens: model.output_tokens(),
                })
                .into(),
            )
            .map_err(|e| HostError::failed(e.to_string()))?;
        let provider_budget = source.recovery.clone().or_else(|| {
            (estimate.input > input_budget(model.as_ref())).then(|| ProviderOutputBudgetFacts {
                input_tokens: estimate.input,
                context_window: model.context_window(),
                requested_max_tokens: model.output_tokens(),
                allowed_max_tokens: (model.context_window() - estimate.input).max(0.),
                ratio: 1.,
            })
        });
        let recovery = provider_budget.is_some();
        if !recovery && estimate.input < model.threshold_tokens() {
            return Ok((view.prepared, estimate.input));
        }
        if !recovery && !self.regrown(&view, model.as_ref()).await? {
            return Ok((view.prepared, estimate.input));
        }
        let step = CompactionStep {
            turn_id: Some(source.turn_id.clone()),
            request_id: Some(source.request_id.clone()),
            command: None,
            through: source.through,
            trigger: if recovery {
                CompactionTrigger::ProviderOutputBudgetExhausted
            } else {
                CompactionTrigger::Threshold
            },
            provider_budget,
        };
        let compacted = self
            .compact_step(
                step,
                &view,
                model.clone(),
                &estimate,
                cancel.clone(),
                cancelled,
            )
            .await?;
        check_cancel(&cancel)?;
        let EventKind::ContextCompaction(data) = &compacted.fact.kind else {
            unreachable!("a compaction step commits a compaction")
        };
        match (&data.outcome, compacted.request) {
            (_, Some(request)) => {
                let request = request?;
                let input = model.count_tokens(&request);
                if recovery && input > input_budget(model.as_ref()) {
                    return Err(recovery_overflow(&data.outcome, model.as_ref(), &view));
                }
                Ok((request, input))
            }
            (ContextCompactionResult::Cancelled { reason }, _) => {
                Err(HostError::Cancelled(reason.clone()))
            }
            (ContextCompactionResult::Failed { error, .. }, _) if recovery => {
                Err(HostError::failed(error.clone()))
            }
            (ContextCompactionResult::Skipped { reason, .. }, _) if recovery => Err(
                HostError::failed(format!("Context recovery could not compact: {reason}")),
            ),
            _ => Ok((view.prepared, estimate.input)),
        }
    }

    /// The one compaction path, for the automatic trigger in preparation and
    /// the Loop's manual step: the start, one strategy call, validation of
    /// any strategy's result, the result, then the optional snapshot.
    /// A cancellation before the strategy returns commits `cancelled` with
    /// `cancelled()`'s reason; a result the strategy returned is kept.
    pub async fn compact_step(
        &self,
        step: CompactionStep,
        view: &View,
        model: Arc<dyn ContextModel>,
        estimate: &Estimate,
        cancel: CancellationToken,
        cancelled: &(dyn Fn() -> String + Sync),
    ) -> Result<Compacted, HostError> {
        let started = Instant::now();
        let turn_id = step.turn_id.as_deref();
        let order = self.cut_order(view, model.as_ref()).await?;
        let carried = turn_id.and_then(|turn| carried_pins(view, turn));
        let turn = turn_id.map(|turn| (turn, carried.as_ref()));
        let manual = step.trigger == CompactionTrigger::Manual;
        let rule = Retention::new(view, model.as_ref(), turn, manual, &order);
        let mut cuts = legal_cuts(&order, &view.history);
        let limit = rule.limit(
            &view.history,
            &cuts,
            self.strategy.replacement_size(model.as_ref()),
        );
        if let Some(limit) = limit {
            for cut in &mut cuts[..=limit] {
                cut.within_limit = true;
            }
        }
        let limit_from = limit.map(|limit| cuts[limit].retain_from.clone());
        let name = self.strategy.name().to_owned();
        let start = self
            .timeline
            .append(
                EventKind::ContextCompactionStart(ContextCompactionStartData {
                    turn_id: step.turn_id.clone(),
                    request_id: step.request_id.clone(),
                    command: step.command,
                    through: step.through,
                    trigger: step.trigger,
                    strategy: name.clone(),
                })
                .into(),
            )
            .map_err(|e| HostError::failed(e.to_string()))?;
        let input = |history: Vec<HistoryEntry>,
                     checkpoint: Option<CompactionCheckpoint>,
                     retain_limit: Option<String>,
                     cuts: Vec<CompactionCut>,
                     prior: Option<StrategyDiagnostics>| CompactionInput {
            history,
            checkpoint,
            request: view.prepared.clone(),
            model: model.clone(),
            retain_limit,
            cuts,
            trigger: step.trigger,
            estimated_input_tokens: estimate.input,
            baseline_source: estimate.source,
            baseline_prompt_tokens: estimate.baseline,
            delta_tokens: estimate.delta,
            provider_budget: step.provider_budget.clone(),
            prior,
        };
        let first_input = input(
            view.history.clone(),
            view.checkpoint.clone(),
            limit_from.clone(),
            cuts.clone(),
            None,
        );
        let record = |retain_from: Option<String>,
                      retain_limit: Option<String>,
                      output: Option<f64>,
                      replaced: usize,
                      diagnostics: StrategyDiagnostics,
                      insufficient_space: Option<InsufficientSpace>| {
            Box::new(CompactionDiagnosticsRecord {
                reason: match step.trigger {
                    CompactionTrigger::Manual => CompactionReason::Manual,
                    _ => CompactionReason::Auto,
                },
                trigger: step.trigger,
                retain_from,
                retain_limit,
                estimated_input_tokens: estimate.input,
                estimated_output_tokens: output,
                entries_replaced: replaced as u64,
                model_call_count: diagnostics.model_call_count,
                duration_ms: started.elapsed().as_millis() as u64,
                model_usage: diagnostics.model_usage,
                insufficient_space,
                strategy: diagnostics.record,
            })
        };
        let completed =
            |composed: Composed, diagnostics: StrategyDiagnostics, retain_limit: Option<String>| {
                let mark = insufficient_space(
                    resident_parts(
                        view,
                        model.as_ref(),
                        &composed.baseline,
                        &composed.replacement,
                        composed.parts,
                    ),
                    composed.tokens,
                    BOUND_SHARE * input_budget(model.as_ref()),
                );
                ContextCompactionResult::Completed {
                    retain_from: composed.retain_from.clone(),
                    replacement: composed.replacement,
                    strategy: name.clone(),
                    diagnostics: record(
                        Some(composed.retain_from),
                        retain_limit,
                        Some(composed.tokens),
                        composed.kept,
                        diagnostics,
                        mark,
                    ),
                    context_baseline: composed.baseline.to_vec(),
                    replacement_parts: composed.parts,
                    pinned_inputs: composed.ids,
                    digest_entries: composed.digest,
                }
            };
        if step.trigger == CompactionTrigger::Threshold
            && first_input
                .cut()
                .is_some_and(|cut| !self.shrinks(view, model.as_ref(), turn, cut))
        {
            let outcome = ContextCompactionResult::Skipped {
                reason: "nothing_to_compact".into(),
                diagnostics: record(
                    None,
                    limit_from.clone(),
                    None,
                    0,
                    StrategyDiagnostics {
                        model_call_count: 0,
                        model_usage: None,
                        record: CompactionStrategyRecord {
                            kind: name.clone(),
                            details: Default::default(),
                        },
                    },
                    None,
                ),
            };
            return self.commit(step, view, model, start, outcome).await;
        }
        let first = self
            .pass(
                &Pass {
                    view,
                    model: model.as_ref(),
                    turn,
                    recovery: step.provider_budget.is_some(),
                    history: &view.history,
                    offset: 0,
                    cuts: &cuts,
                },
                first_input,
                &cancel,
                cancelled,
            )
            .await;
        let outcome = match first {
            StepOutcome::Completed {
                composed,
                diagnostics,
            } => {
                // A summary that overshoots its planned size is kept, so the
                // result can land above the bound with a removable tail left
                // (§5.2, §5.3). The limit is then recomputed from the
                // replacement's actual size, and a later cut re-runs the
                // strategy once as a rolling continuation of this one.
                let redo = (composed.tokens > BOUND_SHARE * input_budget(model.as_ref()))
                    .then(|| replanned(view, model.as_ref(), turn, manual, &composed, &order))
                    .flatten();
                match redo {
                    None => completed(composed, diagnostics, limit_from),
                    Some(redo) => {
                        let redo_limit = Some(redo.cuts[redo.limit].retain_from.clone());
                        let second = self
                            .pass(
                                &Pass {
                                    view,
                                    model: model.as_ref(),
                                    turn: turn_id.map(|turn| (turn, Some(&redo.carried))),
                                    recovery: step.provider_budget.is_some(),
                                    history: &redo.history,
                                    offset: composed.kept,
                                    cuts: &redo.cuts,
                                },
                                input(
                                    redo.history.clone(),
                                    Some(CompactionCheckpoint {
                                        retain_from: composed.retain_from.clone(),
                                        replacement: composed.replacement.clone().into(),
                                        context_baseline: composed.baseline.clone(),
                                        replacement_parts: composed.parts,
                                    }),
                                    redo_limit.clone(),
                                    redo.cuts.clone(),
                                    Some(diagnostics.clone()),
                                ),
                                &cancel,
                                cancelled,
                            )
                            .await;
                        match second {
                            StepOutcome::Completed {
                                composed,
                                diagnostics: second,
                            } => completed(composed, diagnostics.sum(&second), redo_limit),
                            StepOutcome::Skipped { .. } => {
                                completed(composed, diagnostics, limit_from)
                            }
                            StepOutcome::Failed {
                                retain_from,
                                estimated_output_tokens,
                                error,
                                diagnostics: second,
                            } => {
                                let diagnostics = second
                                    .map_or(diagnostics.clone(), |second| diagnostics.sum(&second));
                                ContextCompactionResult::Failed {
                                    error,
                                    diagnostics: Some(record(
                                        retain_from,
                                        redo_limit,
                                        estimated_output_tokens,
                                        0,
                                        diagnostics,
                                        None,
                                    )),
                                }
                            }
                            StepOutcome::Cancelled { reason } => {
                                ContextCompactionResult::Cancelled { reason }
                            }
                        }
                    }
                }
            }
            StepOutcome::Skipped {
                reason,
                diagnostics,
            } => ContextCompactionResult::Skipped {
                reason,
                diagnostics: record(None, limit_from.clone(), None, 0, diagnostics, None),
            },
            StepOutcome::Failed {
                retain_from,
                estimated_output_tokens,
                error,
                diagnostics,
            } => ContextCompactionResult::Failed {
                error,
                diagnostics: diagnostics.map(|diagnostics| {
                    record(
                        retain_from,
                        limit_from.clone(),
                        estimated_output_tokens,
                        0,
                        diagnostics,
                        None,
                    )
                }),
            },
            StepOutcome::Cancelled { reason } => ContextCompactionResult::Cancelled { reason },
        };
        self.commit(step, view, model, start, outcome).await
    }

    /// Whether the entries a compaction at `cut` removes — less the pinned
    /// inputs the replacement carries instead of dropping them, plus the
    /// previous replacement's strategy part, which the next summary reads and
    /// replaces, accounting for the keyed baseline replayed at each cut —
    /// exceed the strategy's planned replacement size
    /// (ContextCompaction §5.3, step 2).
    fn shrinks(
        &self,
        view: &View,
        model: &dyn ContextModel,
        turn: Option<(&str, Option<&CarriedPins>)>,
        cut: &CompactionCut,
    ) -> bool {
        let count =
            |messages: Vec<Message>| model.count_tokens(&ModelRequest::new("", "", messages));
        let empty = count(Vec::new());
        let dropped = count(
            view.history[..cut.entry]
                .iter()
                .map(|entry| entry.message.as_ref().clone())
                .collect(),
        ) - empty;
        let pinned =
            count(pinned_inputs(view, None, turn, &cut.retain_from, model).messages) - empty;
        let previous = view.checkpoint.as_ref().map_or(0., |checkpoint| {
            let len = checkpoint.replacement.len();
            let strategy = checkpoint
                .replacement_parts
                .map_or(len, |parts| parts.strategy as usize)
                .min(len);
            count(
                checkpoint.replacement[..strategy]
                    .iter()
                    .cloned()
                    .chain(
                        checkpoint
                            .context_baseline
                            .iter()
                            .map(|entry| (*entry.message).clone()),
                    )
                    .collect(),
            ) - empty
        });
        let replayed = count(
            view.history[cut.entry - 1]
                .baseline
                .iter()
                .map(|entry| (*entry.message).clone())
                .collect(),
        ) - empty;
        dropped - pinned + previous - replayed > self.strategy.replacement_size(model)
    }

    /// Appends the result of a compaction step, then re-reads the committed
    /// fact for the request it projects and saves the optional snapshot.
    async fn commit(
        &self,
        step: CompactionStep,
        view: &View,
        model: Arc<dyn ContextModel>,
        start: Arc<AgentEvent>,
        outcome: ContextCompactionResult,
    ) -> Result<Compacted, HostError> {
        let fact = self
            .timeline
            .append(
                EventKind::ContextCompaction(ContextCompactionData {
                    turn_id: step.turn_id,
                    request_id: step.request_id,
                    command: step.command,
                    start: Some(start.sequence),
                    through: step.through,
                    outcome,
                })
                .into(),
            )
            .map_err(|e| HostError::failed(e.to_string()))?;
        let EventKind::ContextCompaction(data) = &fact.kind else {
            unreachable!("before preserves event kind")
        };
        let ContextCompactionResult::Completed {
            retain_from,
            replacement,
            context_baseline,
            ..
        } = &data.outcome
        else {
            return Ok(Compacted {
                fact: fact.clone(),
                request: None,
            });
        };
        let request = match self.kept_entry(view, retain_from).await {
            Ok(kept) => {
                let (request, tokens) =
                    view.compacted(model.as_ref(), kept, replacement, context_baseline);
                view.check_compacted(model.as_ref(), tokens)
                    .map(|()| request)
                    .map_err(HostError::failed)
            }
            Err(error) => Err(error),
        };
        if request.is_ok() {
            let saved = async {
                let snapshot = match view.context.full() {
                    Some(context) => Some(self.history.snapshot(context, retain_from, &fact)?),
                    None => {
                        let checkpoint = self
                            .history
                            .stream_checkpoint(&self.timeline, view.through, retain_from, 256)
                            .await
                            .map_err(|error| error.to_string())?;
                        Some(self.history.snapshot_from_checkpoint(&checkpoint, &fact)?)
                    }
                };
                let Some(snapshot) = snapshot else {
                    return Ok(());
                };
                self.timeline
                    .save_snapshot(snapshot)
                    .await
                    .map_err(|error| error.to_string())
            }
            .await;
            if saved.is_err() {
                tracing::warn!("projection snapshot unavailable; using fact replay");
            }
        }
        Ok(Compacted {
            fact: fact.clone(),
            request: Some(request),
        })
    }

    /// One strategy pass on `pass`'s view, composed for the record: a
    /// completed result is validated and pinned, a skipped or failed one
    /// keeps its own diagnostics, and a cancellation writes the caller's
    /// reason.
    async fn pass(
        &self,
        pass: &Pass<'_>,
        input: CompactionInput,
        cancel: &CancellationToken,
        cancelled: &(dyn Fn() -> String + Sync),
    ) -> StepOutcome {
        match self.strategy.compact(input, cancel.clone()).await {
            Ok(StrategyOutcome::Completed {
                retain_from,
                replacement,
                diagnostics,
            }) => {
                let turn_id = pass.turn.map_or("", |(turn_id, _)| turn_id);
                let window = match self.cut_window(pass.view, turn_id, &retain_from).await {
                    Ok(window) => window,
                    Err(error) => {
                        return StepOutcome::Failed {
                            retain_from: Some(retain_from),
                            estimated_output_tokens: None,
                            error: error.to_string(),
                            diagnostics: Some(diagnostics),
                        };
                    }
                };
                match pass.compose(&window, retain_from.clone(), replacement) {
                    Ok(composed) => match pass.view.check_compacted(pass.model, composed.tokens) {
                        Ok(()) => StepOutcome::Completed {
                            composed,
                            diagnostics,
                        },
                        Err(error) => StepOutcome::Failed {
                            retain_from: Some(retain_from),
                            estimated_output_tokens: Some(composed.tokens),
                            error: error.into(),
                            diagnostics: Some(diagnostics),
                        },
                    },
                    Err(error) => StepOutcome::Failed {
                        retain_from: Some(retain_from),
                        estimated_output_tokens: None,
                        error: error.into(),
                        diagnostics: Some(diagnostics),
                    },
                }
            }
            Ok(StrategyOutcome::Skipped {
                reason,
                diagnostics,
            }) => StepOutcome::Skipped {
                reason,
                diagnostics,
            },
            Ok(StrategyOutcome::Failed { error, diagnostics }) => StepOutcome::Failed {
                retain_from: None,
                estimated_output_tokens: None,
                error,
                diagnostics: Some(diagnostics),
            },
            Err(_) if cancel.is_cancelled() => StepOutcome::Cancelled {
                reason: cancelled(),
            },
            Err(error) => StepOutcome::Failed {
                retain_from: None,
                estimated_output_tokens: None,
                error: error.to_string(),
                diagnostics: None,
            },
        }
    }

    /// Fails with budget facts when the trigger's estimate of `request`,
    /// over the branch's committed events, exceeds the input budget.
    pub async fn validate(
        &self,
        request: Arc<ModelRequest>,
        cancel: CancellationToken,
    ) -> Result<(), HostError> {
        let model = self
            .resolver
            .resolve(request.clone(), cancel.clone())
            .await?;
        check_cancel(&cancel)?;
        let prepared = self
            .prepared
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(prepared, _)| Arc::ptr_eq(prepared, &request) || **prepared == *request)
            .map(|(_, input)| *input);
        let input = match prepared {
            Some(input) => input,
            None => {
                let events: Vec<Arc<AgentEvent>> = estimate_events(&self.timeline)
                    .await
                    .map_err(|error| HostError::failed(error.to_string()))?
                    .into_iter()
                    .map(|(_, event)| event)
                    .collect();
                self.estimate(&request, model.as_ref(), &events)?.input
            }
        };
        if input <= input_budget(model.as_ref()) {
            return Ok(());
        }
        Err(HostError::Failed {
            kind: crate::host::HostErrorKind::Other,
            message: "Model input exhausts its context window.".into(),
            budget: Some(ProviderOutputBudgetFacts {
                input_tokens: input,
                context_window: model.context_window(),
                requested_max_tokens: model.output_tokens(),
                allowed_max_tokens: (model.context_window() - input).max(0.),
                ratio: 1.,
            }),
            model: None,
            presentation: None,
        })
    }

    pub fn estimate(
        &self,
        request: &ModelRequest,
        model: &dyn ContextModel,
        events: &[Arc<AgentEvent>],
    ) -> Result<Estimate, HostError> {
        let mut usage: Option<&ModelUsageData> = None;
        for event in events.iter().rev() {
            match &event.kind {
                EventKind::ContextCompaction(data)
                    if usage.is_none()
                        && matches!(data.outcome, ContextCompactionResult::Completed { .. }) =>
                {
                    break;
                }
                EventKind::ModelUsage(data) if usage.is_none() => usage = Some(data),
                EventKind::ModelCall(data)
                    if usage.is_some_and(|u| {
                        u.request_id == data.request_id && u.turn_id == data.turn_id
                    }) =>
                {
                    let value = &usage.expect("matched usage").usage;
                    let previous = &data.request;
                    if value.prompt_tokens > 0.
                        && value.prompt_tokens
                            >= value.cache_read_tokens.unwrap_or(0.)
                                + value.cache_write_tokens.unwrap_or(0.)
                        && previous.provider_id == request.provider_id
                        && previous.model_id == request.model_id
                        && previous.capability_snapshot == request.capability_snapshot
                        && previous.tools == request.tools
                        && previous.system == request.system
                        && previous.config == request.config
                        && previous.metadata == request.metadata
                        && request.messages.starts_with(&previous.messages)
                    {
                        let delta = if previous.messages.len() == request.messages.len() {
                            0.
                        } else {
                            model.count_tokens(&ModelRequest {
                                messages: request.messages[previous.messages.len()..].to_vec(),
                                ..ModelRequest::new(
                                    &request.provider_id,
                                    &request.model_id,
                                    Vec::new(),
                                )
                            })
                        };
                        return Ok(Estimate {
                            input: value.prompt_tokens + delta,
                            source: BaselineSource::ProviderBaseline,
                            baseline: Some(value.prompt_tokens),
                            delta: Some(delta),
                        });
                    }
                    break;
                }
                _ => {}
            }
        }
        Ok(Estimate {
            input: model.count_tokens(request),
            source: BaselineSource::FullEstimate,
            baseline: None,
            delta: None,
        })
    }
}

impl View {
    pub fn events(&self) -> &[Arc<AgentEvent>] {
        &self.events
    }
    /// The request with `history[..kept]` replaced and its token estimate.
    fn compacted(
        &self,
        model: &dyn ContextModel,
        kept: usize,
        replacement: &[Message],
        baseline: &[ContextBaselineEntry],
    ) -> (Arc<ModelRequest>, f64) {
        let request = project(
            &self.request,
            self.prefix_len,
            replacement,
            baseline,
            &self.history[kept..],
        );
        let tokens = model.count_tokens(&request);
        (request, tokens)
    }

    fn check_compacted(&self, model: &dyn ContextModel, tokens: f64) -> Result<(), &'static str> {
        if tokens < model.count_tokens(&self.prepared) {
            if tokens < model.context_window() {
                Ok(())
            } else {
                Err("Compacted context does not fit the model window.")
            }
        } else {
            Err("Compacted context does not reduce input.")
        }
    }
}

/// The replay rule applied to a request: the replacement, the keyed
/// baseline, the kept entries, then what preparation appended.
fn project(
    request: &Arc<ModelRequest>,
    prefix_len: usize,
    replacement: &[Message],
    baseline: &[ContextBaselineEntry],
    kept: &[HistoryEntry],
) -> Arc<ModelRequest> {
    let mut result = (**request).clone();
    result.messages = replacement
        .iter()
        .cloned()
        .map(Arc::new)
        .chain(baseline.iter().map(|entry| entry.message.clone()))
        .chain(kept.iter().map(|entry| entry.message.clone()))
        .chain(request.messages[prefix_len..].iter().cloned())
        .collect();
    Arc::new(result)
}

/// One input the running turn consumed, as the model saw it: its
/// `input/message` id when the record that carried it names it, and the
/// messages the renderer placed for it (ContextCompaction §5.9).
#[derive(Clone)]
struct Pin {
    id: Option<String>,
    messages: Vec<Message>,
}

/// The running turn's pinned messages and, per message, the `input/message`
/// id it came from; absent ids mean the arriving record could not name them
/// (ContextCompaction §5.9).
#[derive(Default)]
struct Pinned {
    messages: Vec<Message>,
    ids: Option<Vec<String>>,
}

/// The pins of the compaction in effect, when it ran in the turn still
/// running: its replacement's pinned part, the ids its record names, and the
/// event index the turn's new inputs are read from — its cut, so nothing
/// before it is read again. An input consumed at the cut itself was kept by
/// that compaction's tail, so a later cut may still replace it and pin it
/// (ContextCompaction §5.3, §5.9).
struct CarriedPins {
    pins: Vec<Pin>,
    /// The event id of the in-effect compaction's cut.
    from: String,
}

impl CarriedPins {
    fn new(
        retain_from: &str,
        replacement: &[Message],
        parts: Option<&ReplacementParts>,
        ids: Option<&[String]>,
    ) -> Self {
        let pinned = parts.map_or(&[][..], |parts| {
            let from = ((parts.strategy + parts.digest) as usize).min(replacement.len());
            &replacement[from..]
        });
        let ids = ids.filter(|ids| ids.len() == pinned.len());
        let pins = group_pins(pinned, ids);
        Self {
            pins,
            from: retain_from.to_owned(),
        }
    }
}

/// Pins grouped from stored messages and their per-message ids: consecutive
/// messages with the same id are one input; without ids each message stands
/// alone, and a truncation of it has no handle to name.
fn group_pins(messages: &[Message], ids: Option<&[String]>) -> Vec<Pin> {
    let mut pins: Vec<Pin> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let id = ids.map(|ids| ids[index].clone());
        match pins.last_mut() {
            Some(last) if id.is_some() && last.id == id => last.messages.push(message.clone()),
            _ => pins.push(Pin {
                id,
                messages: vec![message.clone()],
            }),
        }
    }
    pins
}

/// The digest for a cut (ContextCompaction §5.10): the entries of the
/// compaction in effect rolled forward from its record, then the declarations
/// of the `step/tool-result` events between that record's cut and this one,
/// one entry per path or command with its latest outcome. Beyond
/// `DIGEST_SHARE` of the input budget the least recently declared entries are
/// dropped, and `dropped` stays set once anything was; with every entry
/// dropped the note alone is the digest, written even when it alone is over
/// the bound so that later digests keep it. None when the cut has no event in
/// the view, or when the record's cut is one the view can place neither among
/// its events nor as the compaction it applies.
fn digest_entries(
    view: &View,
    window: &CutWindow,
    model: &dyn ContextModel,
) -> Option<DigestEntries> {
    // The compaction in effect is the latest completed record of the view,
    // wherever the new cut falls: a record is written after the events it
    // keeps, so it can sit after the new cut, and a retry reads records from
    // the reusable events too.
    let previous = view
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(data)
                if matches!(data.outcome, ContextCompactionResult::Completed { .. }) =>
            {
                Some(data)
            }
            _ => None,
        });
    {
        let (mut entries, mut dropped) = match previous {
            Some(data) => {
                let ContextCompactionResult::Completed { digest_entries, .. } = &data.outcome
                else {
                    unreachable!("matched a completed compaction")
                };
                (
                    digest_entries
                        .as_ref()
                        .map(|digest| digest.entries.clone())
                        .unwrap_or_default(),
                    digest_entries.as_ref().is_some_and(|digest| digest.dropped),
                )
            }
            None => (Vec::new(), false),
        };
        // The window places the cut and every tool result by chain position: it
        // merges ancestor and current-branch results, so raw sequences do not
        // order them. A result at or after the cut is not part of it.
        let rank = |branch: &str| view.context.branches.iter().position(|name| name == branch);
        let cut = window
            .cut_position
            .as_ref()
            .map(|cut| (rank(&cut.branch_id), cut.sequence));
        for (position, event) in &window.tool_results {
            let after_cut =
                cut.is_none_or(|cut| (rank(&position.branch_id), position.sequence) >= cut);
            if after_cut {
                continue;
            }
            let EventKind::ToolResult(data) = &event.kind else {
                continue;
            };
            for result in &data.results {
                for effect in result.result.effects.iter().flatten() {
                    let entry = effect.shortened();
                    let key = digest_key(&entry);
                    if let Some(index) = entries
                        .iter()
                        .position(|existing| digest_key(existing) == key)
                    {
                        entries.remove(index);
                    }
                    entries.push(entry);
                }
            }
        }
        let empty = model.count_tokens(&ModelRequest::new("", "", Vec::new()));
        let count = |entries: &[ToolEffect], dropped: bool| {
            model.count_tokens(&ModelRequest::new(
                "",
                "",
                vec![digest_message(entries, dropped)],
            )) - empty
        };
        let limit = DIGEST_SHARE * input_budget(model);
        if (dropped || !entries.is_empty()) && count(&entries, dropped) > limit {
            dropped = true;
            let fits = |kept: usize| count(&entries[kept..], true) <= limit;
            let (mut low, mut high) = (0, entries.len());
            if fits(high) {
                while high - low > 1 {
                    let middle = low + (high - low) / 2;
                    if fits(middle) {
                        high = middle;
                    } else {
                        low = middle;
                    }
                }
            }
            entries = entries.split_off(high);
        }
        (dropped || !entries.is_empty()).then_some(DigestEntries { entries, dropped })
    }
}

/// An entry's key: its kind together with its path or shortened command.
fn digest_key(effect: &ToolEffect) -> (u8, &str) {
    match effect {
        ToolEffect::FileWritten { path } => (0, path),
        ToolEffect::CommandRun { command, .. } => (1, command),
    }
}

/// The pins of the latest completed compaction in the view, when it ran in
/// the turn still running; otherwise the turn's pins start at its own
/// `turn/start` (ContextCompaction §5.9). A snapshot is only a cache, so
/// this lookup does not depend on one.
fn carried_pins(view: &View, turn_id: &str) -> Option<CarriedPins> {
    let data = view
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(data)
                if data.turn_id.as_deref() == Some(turn_id)
                    && matches!(data.outcome, ContextCompactionResult::Completed { .. }) =>
            {
                Some(data)
            }
            _ => None,
        })?;
    let ContextCompactionResult::Completed {
        retain_from,
        replacement,
        replacement_parts,
        pinned_inputs,
        ..
    } = &data.outcome
    else {
        unreachable!("matched a completed compaction")
    };
    Some(CarriedPins::new(
        retain_from,
        replacement,
        replacement_parts.as_ref(),
        pinned_inputs.as_deref(),
    ))
}

/// The running turn's inputs the cut replaces, as the model saw them
/// (ContextCompaction §5.9): the pins the compaction in effect already
/// carries, then the inputs the turn consumed at or after that compaction's
/// cut and before `retain_from`, in event order. Without one, they are every
/// input the turn consumed since its `turn/start`. The user content and every
/// keyless raw block of each input, as the renderer places it; keyed blocks
/// are excluded, because the keyed baseline replays them. The messages take
/// at most `PINNED_SHARE` of the input budget, beyond which each input keeps
/// its head and tail with a marker naming its `input/message` event id.
fn pinned_inputs(
    view: &View,
    window: Option<&CutWindow>,
    turn: Option<(&str, Option<&CarriedPins>)>,
    retain_from: &str,
    model: &dyn ContextModel,
) -> Pinned {
    let Some((turn_id, carried)) = turn else {
        return Pinned::default();
    };
    let Some(window) = window else {
        return Pinned::default();
    };
    let events = &window.events;
    let Some(cut) = events.iter().position(|event| event.id == retain_from) else {
        return Pinned::default();
    };
    let start = events[..cut].iter().rposition(
        |event| matches!(&event.kind, EventKind::TurnStart(data) if data.turn_id == turn_id),
    );
    let carried_from =
        carried.and_then(|carried| events.iter().position(|event| event.id == carried.from));
    let from = match (carried_from, start) {
        (Some(carried), Some(start)) => carried.max(start + 1),
        (Some(carried), None) => carried,
        (None, Some(start)) => start + 1,
        (None, None) => return Pinned::default(),
    };
    let messages: HashMap<&str, &InputMessageData> = events[..cut]
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => Some((event.id.as_str(), data)),
            _ => None,
        })
        .collect();
    let checkpointed = view
        .context
        .full()
        .and_then(|context| context.checkpoint.as_ref())
        .map(|checkpoint| checkpoint.state.inputs.as_slice())
        .unwrap_or_default();
    let mut pins = carried
        .map(|carried| carried.pins.clone())
        .unwrap_or_default();
    for event in &events[from.min(cut)..cut] {
        let EventKind::InputConsumed(data) = &event.kind else {
            continue;
        };
        let input = messages.get(data.input_id.as_str()).copied().or_else(|| {
            checkpointed
                .iter()
                .find(|(id, _)| id == &data.input_id)
                .map(|(_, input)| input)
        });
        let Some(input) = input else {
            continue;
        };
        let mut rendered = Vec::new();
        for block in &input.blocks {
            if let InputBlock::Raw {
                text, key: None, ..
            } = block
            {
                rendered.push(Message::text(Role::User, text));
            }
        }
        if let Some(user) = &input.user {
            rendered.push(user_message(user));
        }
        if !rendered.is_empty() {
            pins.push(Pin {
                id: Some(data.input_id.clone()),
                messages: rendered,
            });
        }
    }
    if pins.is_empty() {
        return Pinned::default();
    }
    let count = |messages: &[Message]| {
        model.count_tokens(&ModelRequest::new("", "", messages.to_vec()))
            - model.count_tokens(&ModelRequest::new("", "", Vec::new()))
    };
    let limit = PINNED_SHARE * input_budget(model);
    let total: f64 = pins.iter().map(|pin| count(&pin.messages)).sum();
    let share = if total <= limit {
        limit
    } else {
        limit / pins.len() as f64
    };
    let mut messages = Vec::new();
    let mut ids = Vec::new();
    for pin in pins {
        if count(&pin.messages) <= share {
            ids.extend(std::iter::repeat_n(pin.id.clone(), pin.messages.len()));
            messages.extend(pin.messages);
        } else {
            let text = pin_text(&pin.messages);
            messages.push(truncate_pin(pin.id.as_deref(), &text, share, model));
            ids.push(pin.id);
        }
    }
    Pinned {
        messages,
        ids: ids.into_iter().collect(),
    }
}

/// The text of one input's messages, without the handle a previous
/// truncation appended: the truncation here adds its own.
fn pin_text(messages: &[Message]) -> String {
    let text = messages
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
        .join("\n");
    match text.rfind("\n[input ") {
        Some(index) if text[index + 1..].ends_with(']') && !text[index + 1..].contains('\n') => {
            text[..index].to_owned()
        }
        _ => text,
    }
}

/// One input's head and tail within `share`, with a marker naming the
/// `input/message` event id Recall opens when the input has one; a share too
/// small for the marker keeps the marker alone.
fn truncate_pin(id: Option<&str>, text: &str, share: f64, model: &dyn ContextModel) -> Message {
    let build = |keep: usize| {
        let body = truncate_middle(text, keep);
        Message::text(
            Role::User,
            match id {
                Some(id) => format!("{body}\n[input {id}]"),
                None => body,
            },
        )
    };
    let count = |message: &Message| {
        model.count_tokens(&ModelRequest::new("", "", vec![message.clone()]))
            - model.count_tokens(&ModelRequest::new("", "", Vec::new()))
    };
    if count(&build(0)) > share {
        return match id {
            Some(id) => Message::text(Role::User, format!("[input {id}]")),
            None => build(0),
        };
    }
    let (mut low, mut high) = (0, text.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        if count(&build(middle)) <= share {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    build(low)
}

/// Whether at least `RETAINED_SHARE × B` of new history has accumulated
/// since the branch's latest compaction attempt, of any trigger and any
/// outcome (ContextCompaction §5.6): the entries placed by events after that
/// attempt's start `through`, counted with `count_tokens` each and summed. A
/// branch with no start has all its history new. Automatic compaction runs
/// only when this holds; budget recovery is exempt.
///
/// `through` is a sequence of the branch that wrote the start, so it is
/// mapped through `positions` to that branch's last event at or before it,
/// and an entry is new when its own event follows that one in the context's
/// order: a child branch's smaller local sequences are still new after an
/// inherited attempt.
fn regrown(view: &View, start: Option<(&TimelinePosition, u64)>, model: &dyn ContextModel) -> bool {
    let Some((at, through)) = start else {
        return true;
    };
    let rank = |branch: &str| view.context.branches.iter().position(|name| name == branch);
    let boundary = rank(&at.branch_id);
    let count = |messages: Vec<Message>| model.count_tokens(&ModelRequest::new("", "", messages));
    let empty = count(Vec::new());
    let new: f64 = view
        .history
        .iter()
        .filter(|entry| match rank(&entry.position.branch_id) {
            Some(index) => match boundary {
                Some(boundary) => {
                    index > boundary || (index == boundary && entry.position.sequence > through)
                }
                None => true,
            },
            None => true,
        })
        .map(|entry| count(vec![entry.message.as_ref().clone()]) - empty)
        .sum();
    new >= RETAINED_SHARE * input_budget(model)
}

/// The input budget `context_window − output_tokens`.
fn input_budget(model: &dyn ContextModel) -> f64 {
    (model.context_window() - model.output_tokens()).max(0.)
}

/// The tokens `history[index..]` counts for every index.
fn tail_tokens(history: &[HistoryEntry], empty: f64, model: &dyn ContextModel) -> Vec<f64> {
    let mut tail = vec![0.; history.len() + 1];
    for (index, entry) in history.iter().enumerate().rev() {
        let message = ModelRequest::new("", "", vec![entry.message.as_ref().clone()]);
        tail[index] = tail[index + 1] + model.count_tokens(&message) - empty;
    }
    tail
}

/// The history entry after which a turn start can be the first cut the
/// retention rule keeps: the last entry whose retained tail still exceeds
/// `RETAINED_SHARE` of the input budget. The rule keeps the first cut whose
/// tail fits `min(RETAINED_SHARE × B, BOUND_SHARE × B − resident)`; a cut
/// keeping fewer entries keeps a larger tail too, so no cut before this
/// position can be kept (ContextCompaction §5.3).
fn cut_window<'a>(
    history: &'a [HistoryEntry],
    model: &dyn ContextModel,
) -> Option<&'a HistoryEntry> {
    let empty = model.count_tokens(&ModelRequest::new("", "", Vec::new()));
    let tail = tail_tokens(history, empty, model);
    let budget = RETAINED_SHARE * input_budget(model);
    let last = tail[..history.len()]
        .iter()
        .rposition(|tokens| *tokens > budget)?;
    history.get(last)
}

/// The retention rule of one compaction step (ContextCompaction §5.3): the
/// request frame and the running turn's pins are fixed for the step, so the
/// rule measures them once and answers each cut with its resident parts.
struct Retention<'a> {
    view: &'a View,
    model: &'a dyn ContextModel,
    /// The running turn and the pins it already carries (§5.9).
    turn: Option<(&'a str, Option<&'a CarriedPins>)>,
    manual: bool,
    /// The order the step's cuts come from: its turn starts are the manual
    /// limit's candidate.
    order: &'a CutOrder,
    empty: f64,
    frame: f64,
}

impl<'a> Retention<'a> {
    fn new(
        view: &'a View,
        model: &'a dyn ContextModel,
        turn: Option<(&'a str, Option<&'a CarriedPins>)>,
        manual: bool,
        order: &'a CutOrder,
    ) -> Self {
        let empty = model.count_tokens(&ModelRequest::new("", "", Vec::new()));
        // The frame is the same at every cut: the system prompt, the tool
        // declarations and the preparation's appended messages.
        let mut frame = (*view.request).clone();
        frame.messages = view
            .request
            .messages
            .get(view.prefix_len..)
            .unwrap_or_default()
            .to_vec();
        Self {
            view,
            model,
            turn,
            manual,
            order,
            empty,
            frame: model.count_tokens(&frame),
        }
    }

    /// The tokens a compaction at `cut` cannot shrink: the frame, the keyed
    /// baseline at the cut, the pinned inputs at the cut's own size, the
    /// digest at its bound (its entries depend on the cut being chosen,
    /// §5.10) and the strategy's replacement.
    fn resident(
        &self,
        history: &[HistoryEntry],
        cut: &CompactionCut,
        replacement_size: f64,
    ) -> f64 {
        let count = |messages: Vec<Message>| {
            self.model
                .count_tokens(&ModelRequest::new("", "", messages))
        };
        let baseline = count(
            history[cut.entry - 1]
                .baseline
                .iter()
                .map(|entry| entry.message.as_ref().clone())
                .collect(),
        ) - self.empty;
        let pins =
            count(pinned_inputs(self.view, None, self.turn, &cut.retain_from, self.model).messages)
                - self.empty;
        self.frame + baseline + pins + DIGEST_SHARE * input_budget(self.model) + replacement_size
    }

    /// The tokens `messages` count, beyond the empty list's own bytes.
    fn tokens(&self, messages: &[Message]) -> f64 {
        self.model
            .count_tokens(&ModelRequest::new("", "", messages.to_vec()))
            - self.empty
    }

    /// The index in `cuts` of the earliest cut whose kept tail fits
    /// `min(RETAINED_SHARE × B, BOUND_SHARE × B − resident)`; for a manual
    /// step, the latest turn's start when it keeps less. The cut that
    /// replaces everything fits whenever the resident parts fit half the
    /// budget, so the compaction reaches the bound by keeping no tail
    /// (§5.3). None when no cut qualifies.
    fn limit(
        &self,
        history: &[HistoryEntry],
        cuts: &[CompactionCut],
        replacement_size: f64,
    ) -> Option<usize> {
        let tail = tail_tokens(history, self.empty, self.model);
        let budget = input_budget(self.model);
        let fits = cuts.iter().position(|cut| {
            let resident_tokens = self.resident(history, cut, replacement_size);
            tail[cut.entry] <= (RETAINED_SHARE * budget).min(BOUND_SHARE * budget - resident_tokens)
        });
        let latest_turn = self
            .manual
            .then(|| {
                let start = self.order.events.iter().rfind(|event| event.turn_start)?;
                cuts.iter().position(|cut| cut.retain_from == start.id)
            })
            .flatten();
        fits.max(latest_turn)
    }
}

/// A re-plan of a completed compaction that overshoots the bound: the
/// entries from the first result's cut on, its legal cuts marked against a
/// limit recomputed from the replacement's actual size, that limit's index
/// and the pins the first result already carries (ContextCompaction §5.3).
struct Replan {
    history: Vec<HistoryEntry>,
    cuts: Vec<CompactionCut>,
    limit: usize,
    carried: CarriedPins,
}

/// The re-plan of `first`, or None when its overshoot is not curable by a
/// later cut: no cut fits the bound at the replacement's actual size, or the
/// limit it allows is not later than the first result's cut.
fn replanned(
    view: &View,
    model: &dyn ContextModel,
    turn: Option<(&str, Option<&CarriedPins>)>,
    manual: bool,
    first: &Composed,
    order: &CutOrder,
) -> Option<Replan> {
    let carried = CarriedPins::new(
        &first.retain_from,
        &first.replacement,
        first.parts.as_ref(),
        first.ids.as_deref(),
    );
    let rule = Retention::new(
        view,
        model,
        turn.map(|(turn, _)| (turn, Some(&carried))),
        manual,
        order,
    );
    let history = &view.history[first.kept..];
    let mut cuts = legal_cuts(order, history);
    let strategy = first
        .parts
        .map_or(first.replacement.len(), |parts| parts.strategy as usize)
        .min(first.replacement.len());
    let limit = rule.limit(history, &cuts, rule.tokens(&first.replacement[..strategy]))?;
    if cuts[limit].entry == 0 {
        return None;
    }
    for cut in &mut cuts[..=limit] {
        cut.within_limit = true;
    }
    Some(Replan {
        history: history.to_vec(),
        cuts,
        limit,
        carried,
    })
}

/// The resident parts of a completed compaction, measured on the result
/// (ContextCompaction §5.3 step 5): the frame, the keyed baseline, and the
/// replacement's summary, digest and pinned inputs at their actual sizes.
fn resident_parts(
    view: &View,
    model: &dyn ContextModel,
    baseline: &[ContextBaselineEntry],
    replacement: &[Message],
    parts: Option<ReplacementParts>,
) -> Vec<(InsufficientSpacePartName, f64)> {
    let count = |messages: Vec<Message>| model.count_tokens(&ModelRequest::new("", "", messages));
    let base = count(Vec::new());
    let only = |system: Option<Arc<SystemPrompt>>, tools: Option<Arc<Vec<ToolDefinition>>>| {
        let mut request = ModelRequest::new("", "", Vec::new());
        request.system = system;
        request.tools = tools;
        model.count_tokens(&request) - base
    };
    let (strategy, digest) = parts.map_or((replacement.len(), 0), |parts| {
        (
            (parts.strategy as usize).min(replacement.len()),
            parts.digest as usize,
        )
    });
    let after_digest = (strategy + digest).min(replacement.len());
    vec![
        (
            InsufficientSpacePartName::SystemPrompt,
            only(view.request.system.clone(), None),
        ),
        (
            InsufficientSpacePartName::Tools,
            only(None, view.request.tools.clone()),
        ),
        (
            InsufficientSpacePartName::AppendedMessages,
            count(
                view.request.messages[view.prefix_len..]
                    .iter()
                    .map(|message| (**message).clone())
                    .collect(),
            ) - base,
        ),
        (
            InsufficientSpacePartName::KeyedBaseline,
            count(
                baseline
                    .iter()
                    .map(|entry| entry.message.as_ref().clone())
                    .collect(),
            ) - base,
        ),
        (
            InsufficientSpacePartName::Summary,
            count(replacement[..strategy].to_vec()) - base,
        ),
        (
            InsufficientSpacePartName::Digest,
            count(replacement[strategy..after_digest].to_vec()) - base,
        ),
        (
            InsufficientSpacePartName::PinnedInputs,
            count(replacement[after_digest..].to_vec()) - base,
        ),
    ]
}

/// The mark of a completed compaction whose request exceeds `bound`: the
/// named parts, largest first, until their sizes cover the excess
/// (ContextCompaction §5.3 step 5). The excess is measured from the total
/// the record reports, so a result whose resident parts fit but whose kept
/// tail, or a summary beyond its planned size, leaves it above the bound is
/// marked too; there the largest parts still cover the excess.
fn insufficient_space(
    parts: Vec<(InsufficientSpacePartName, f64)>,
    total: f64,
    bound: f64,
) -> Option<InsufficientSpace> {
    let mut excess: f64 = total - bound;
    if excess <= 0. {
        return None;
    }
    let mut sorted = parts;
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut marked = Vec::new();
    for (part, tokens) in sorted {
        if excess <= 0. {
            break;
        }
        marked.push(InsufficientSpacePart { part, tokens });
        excess -= tokens;
    }
    Some(InsufficientSpace { parts: marked })
}

/// A terminal recovery error naming the resident parts or uncuttable
/// entries that still exceed the input budget (ContextCompaction §5.3).
fn recovery_overflow(
    outcome: &ContextCompactionResult,
    model: &dyn ContextModel,
    view: &View,
) -> HostError {
    let message = match outcome {
        ContextCompactionResult::Completed {
            replacement,
            context_baseline,
            replacement_parts,
            ..
        } => {
            let parts = resident_parts(
                view,
                model,
                context_baseline,
                replacement,
                *replacement_parts,
            );
            let resident: f64 = parts.iter().map(|(_, tokens)| tokens).sum();
            match insufficient_space(parts, resident, input_budget(model)) {
                Some(mark) => format!(
                    "Context recovery could not fit the request; too large: {}",
                    mark.parts
                        .iter()
                        .map(|part| format!("{} {:.0} tokens", part.part.name(), part.tokens))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => "Context recovery could not fit the request; the entries the last \
                         event placed cannot be cut."
                    .into(),
            }
        }
        _ => "Context recovery could not fit the request.".into(),
    };
    HostError::failed(message)
}

/// Every event boundary that replaces at least one entry without separating
/// a tool call from its results or context: turn starts, the events that
/// place entries and the boundary's own last event (replacing everything),
/// in order. None is marked within the retention limit yet.
fn legal_cuts(order: &CutOrder, history: &[HistoryEntry]) -> Vec<CompactionCut> {
    let rank = |branch: &str| order.branches.iter().position(|name| name == branch);
    let position = |branch: &str, sequence: u64| (rank(branch).unwrap_or(usize::MAX), sequence);
    let mut candidates: Vec<((usize, u64), String)> = order
        .events
        .iter()
        .filter(|event| event.turn_start || event.boundary)
        .map(|event| {
            (
                position(&event.position.branch_id, event.position.sequence),
                event.id.clone(),
            )
        })
        .chain(history.iter().map(|entry| {
            (
                position(&entry.position.branch_id, entry.position.sequence),
                entry.event_id.clone(),
            )
        }))
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    candidates.dedup_by(|a, b| a.1 == b.1);
    let mut cuts = Vec::new();
    for (at, id) in candidates {
        let kept = history
            .iter()
            .filter(|entry| position(&entry.position.branch_id, entry.position.sequence) < at)
            .count();
        if kept == 0 || !separates_nothing(history, kept) {
            continue;
        }
        cuts.push(CompactionCut {
            retain_from: id,
            entry: kept,
            within_limit: false,
        });
    }
    cuts
}

/// No kept entry closes a replaced tool call, and the first kept entry is
/// not the context of a replaced result.
fn separates_nothing(history: &[HistoryEntry], kept: usize) -> bool {
    let replaced: std::collections::HashSet<_> = history[..kept]
        .iter()
        .flat_map(|entry| match &entry.message.content {
            MessageContent::Parts(parts) => parts.iter(),
            MessageContent::Text(_) => [].iter(),
        })
        .filter_map(|part| match part {
            MessagePart::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    !history[kept..]
        .iter()
        .any(|entry| match &entry.message.content {
            MessageContent::Parts(parts) => parts.iter().any(|part| {
                matches!(part, MessagePart::ToolResult { tool_use_id, .. }
                if replaced.contains(tool_use_id.as_str()))
            }),
            MessageContent::Text(_) => false,
        })
        && !history.get(kept).is_some_and(|entry| entry.tool_context)
}

pub(crate) fn check_cancel(cancel: &CancellationToken) -> Result<(), HostError> {
    if cancel.is_cancelled() {
        Err(HostError::Cancelled("Operation cancelled".into()))
    } else {
        Ok(())
    }
}

/// Whether `left` sits before `right` in the branch chain.
fn earlier(branches: &[String], left: &TimelinePosition, right: &TimelinePosition) -> bool {
    let rank = |branch: &str| branches.iter().position(|name| name == branch);
    (rank(&left.branch_id), left.sequence) < (rank(&right.branch_id), right.sequence)
}

/// Whether `left` sits after `right` in the branch chain.
fn later(branches: &[String], left: &TimelinePosition, right: &TimelinePosition) -> bool {
    earlier(branches, right, left)
}

/// The facts a request's estimate compares against, oldest first on the
/// chain: the newest provider usage, the model call it measured, the newest
/// completed compaction and the newest adopted configuration. One page each.
pub(crate) async fn estimate_events(
    timeline: &Timeline,
) -> dim_agent_runtime::Result<Vec<EstimateEvent>> {
    let mut facts: Vec<EstimateEvent> = Vec::new();
    let usage = timeline
        .query(newest_chain(Some("model/usage"), None, None, 1))
        .await?;
    let Some((usage_event, usage_position)) = usage.events.into_iter().zip(usage.positions).next()
    else {
        return Ok(Vec::new());
    };
    let EventKind::ModelUsage(data) = &usage_event.kind else {
        unreachable!("the read selects one model/usage")
    };
    let request_id = data.request_id.clone();
    facts.push((usage_position, usage_event));
    let call = timeline
        .query(newest_chain(
            Some("model/call"),
            None,
            Some(TimelineQueryPredicate {
                op: PredicateOp::Eq,
                path: "request_id".into(),
                value: json!(request_id),
            }),
            1,
        ))
        .await?;
    facts.extend(call.positions.into_iter().zip(call.events));
    let compaction = timeline
        .query(newest_chain(
            Some("context/compaction"),
            None,
            Some(TimelineQueryPredicate {
                op: PredicateOp::Eq,
                path: "status".into(),
                value: json!("completed"),
            }),
            1,
        ))
        .await?;
    facts.extend(compaction.positions.into_iter().zip(compaction.events));
    let configured = timeline
        .query(newest_chain(Some("context/configured"), None, None, 1))
        .await?;
    facts.extend(configured.positions.into_iter().zip(configured.events));
    let branches: Vec<String> = timeline
        .ancestors()
        .into_iter()
        .map(|(branch, _)| branch)
        .chain(std::iter::once(timeline.branch_id()))
        .collect();
    facts.sort_by_key(|(position, _)| {
        let rank = branches
            .iter()
            .position(|branch| branch == &position.branch_id);
        (rank, position.sequence)
    });
    Ok(facts)
}

/// The same facts a decoded context already holds, oldest first.
pub(crate) fn estimate_events_in(context: &ContextHistory) -> Vec<EstimateEvent> {
    context
        .events
        .iter()
        .zip(context.positions.iter())
        .filter(|(event, _)| {
            matches!(
                &event.kind,
                EventKind::ContextConfigured(_)
                    | EventKind::ContextCompaction(_)
                    | EventKind::ModelUsage(_)
                    | EventKind::ModelCall(_)
            )
        })
        .map(|(event, position)| (position.clone(), event.clone()))
        .collect()
}

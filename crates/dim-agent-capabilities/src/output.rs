use crate::{host::*, steer::REPLACED_BY_STEER};
use dim_agent_runtime::{ModelProgress, StateCell, Timeline, ToolProgress};
use dim_agent_types::*;
use futures_util::StreamExt;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Text of a block is committed at the block end, or earlier once this much
/// is buffered (ADR agent-029, implementation record).
const TEXT_FLUSH_BYTES: usize = 4 * 1024;
/// ... or once the oldest buffered text is this old, checked as text arrives.
const TEXT_FLUSH_AGE: Duration = Duration::from_secs(1);

/// Progress that an event now holds; it leaves the progress once that event
/// is published, so a client never sees neither (ADR agent-029, decision 2).
enum Handoff {
    /// The first bytes of the progress text.
    Text(usize),
    /// The first bytes of the progress thinking.
    Thinking(usize),
    Tool(String),
}

/// Whether a request received visible model output or was replaced by a
/// steer first; whichever comes first holds (ADR agent-029 decision 5).
#[derive(Default)]
pub(crate) struct OutputGate(AtomicU8);
const WAITING: u8 = 0;
const RECEIVED: u8 = 1;
const REPLACED: u8 = 2;
impl OutputGate {
    /// Replaces the request unless it already received visible output.
    pub fn replace(&self) -> bool {
        matches!(
            self.0
                .compare_exchange(WAITING, REPLACED, Ordering::AcqRel, Ordering::Acquire),
            Ok(_) | Err(REPLACED)
        )
    }
    /// Marks visible output received; false once the request was replaced.
    fn receive(&self) -> bool {
        !matches!(
            self.0
                .compare_exchange(WAITING, RECEIVED, Ordering::AcqRel, Ordering::Acquire),
            Err(REPLACED)
        )
    }
    pub fn replaced(&self) -> bool {
        self.0.load(Ordering::Acquire) == REPLACED
    }
}

/// A call of this request whose `step/tool-use-start` is committed.
pub(crate) struct StartedCall {
    pub id: String,
    /// Its `step/tool-use` is committed too.
    pub complete: bool,
}

/// The single result batch closing every started call of an output that did
/// not complete, in start order (ADR agent-029 decision 3).
pub(crate) fn closure(
    turn_id: &str,
    request_id: &str,
    started: &[StartedCall],
) -> Option<ToolResultData> {
    (!started.is_empty()).then(|| ToolResultData {
        turn_id: turn_id.to_owned(),
        request_id: request_id.to_owned(),
        call_event_id: None,
        results: started
            .iter()
            .map(|call| ToolCallResult {
                call_id: call.id.clone(),
                result: ToolResult::error(if call.complete {
                    "Tool was not executed: the model output ended"
                } else {
                    "Tool was not executed: the model output ended before its arguments were complete"
                }),
            })
            .collect(),
    })
}

/// What one request committed to the Timeline, and its progress: received
/// output not committed yet.
pub(crate) struct Output {
    timeline: Arc<Timeline>,
    gate: Arc<OutputGate>,
    progress: Arc<StateCell<ModelProgress>>,
    scope: ModelCallData,
    /// Text of the current block not committed yet.
    text: String,
    /// Stable identity of the current text block, across flush chunks.
    text_part_id: Option<String>,
    /// Whether the first text event carrying `text_part_id` was committed.
    text_part_committed: bool,
    /// Stable identity of the current thinking block.
    thinking_part_id: Option<String>,
    /// Whether the first thinking event carrying `thinking_part_id` was committed.
    thinking_part_committed: bool,
    text_since: Option<Instant>,
    pub started: Vec<StartedCall>,
    committed_text: String,
    committed_thinking: String,
    committed_calls: HashMap<String, ToolCall>,
    /// Committed output still shown as progress, by event sequence.
    handoffs: VecDeque<(u64, Handoff)>,
}
impl Output {
    pub fn new(
        timeline: Arc<Timeline>,
        gate: Arc<OutputGate>,
        progress: Arc<StateCell<ModelProgress>>,
        scope: ModelCallData,
    ) -> Self {
        progress.set(ModelProgress {
            turn_id: Some(scope.turn_id.clone()),
            request_id: Some(scope.request_id.clone()),
            ..ModelProgress::default()
        });
        Self {
            timeline,
            gate,
            progress,
            scope,
            text: String::new(),
            text_part_id: None,
            text_part_committed: false,
            thinking_part_id: None,
            thinking_part_committed: false,
            text_since: None,
            started: Vec::new(),
            committed_text: String::new(),
            committed_thinking: String::new(),
            committed_calls: HashMap::new(),
            handoffs: VecDeque::new(),
        }
    }
    /// Changes the progress while it still shows this request; a request
    /// started later owns it otherwise.
    /// Published output leaves the progress first.
    fn show(&mut self, change: impl FnOnce(&mut ModelProgress)) {
        let published = self.timeline.sequence();
        let mut due = Vec::new();
        while self
            .handoffs
            .front()
            .is_some_and(|(sequence, _)| *sequence <= published)
        {
            due.push(self.handoffs.pop_front().expect("checked front").1);
        }
        self.progress.update(|progress| {
            if progress.request_id.as_deref() == Some(self.scope.request_id.as_str()) {
                for handoff in due {
                    match handoff {
                        Handoff::Text(len) => {
                            progress.text_offset += progress.text.drain(..len).count() as u64
                        }
                        Handoff::Thinking(len) => {
                            progress.thinking_offset +=
                                progress.thinking.drain(..len).count() as u64
                        }
                        Handoff::Tool(id) => progress.tools.retain(|tool| tool.call_id != id),
                    }
                }
                change(progress);
            }
        });
    }
    /// Bytes at the start of the progress text (or thinking) that committed
    /// events still hold.
    fn handed_off(&self, thinking: bool) -> usize {
        self.handoffs
            .iter()
            .map(|(_, handoff)| match (handoff, thinking) {
                (Handoff::Text(len), false) | (Handoff::Thinking(len), true) => *len,
                _ => 0,
            })
            .sum()
    }
    /// The progress after its handed-off part becomes `tail`: what a just
    /// committed event holds, so the offsets count exactly the published
    /// deltas (ADR agent-029 decision 2).
    fn replace_tail(&self, thinking: bool, tail: &str) {
        let earlier = self.handed_off(thinking);
        self.progress.update(|progress| {
            if progress.request_id.as_deref() == Some(self.scope.request_id.as_str()) {
                if thinking {
                    progress.text_part_id = None;
                    progress.thinking_part_id = self.thinking_part_id.clone();
                    progress.thinking.truncate(earlier);
                    progress.thinking.push_str(tail);
                } else {
                    progress.thinking_part_id = None;
                    progress.text_part_id = self.text_part_id.clone();
                    progress.text.truncate(earlier);
                    progress.text.push_str(tail);
                }
            }
        });
    }
    /// Its output stays shown until the events holding it are published.
    fn hand_off(&mut self, sequence: u64, handoff: Handoff) {
        self.handoffs.push_back((sequence, handoff));
        self.show(|_| {});
    }
    /// The request wrote its terminal: once everything it committed is
    /// published, its progress is gone.
    /// Unloading stops the wait: its progress goes with this generation.
    pub async fn release(&mut self, unloading: &CancellationToken) {
        let target = self.timeline.committed();
        let mut published = self.timeline.watch();
        while !published.borrow().1 && published.borrow().0 < target {
            tokio::select! {
                changed = published.changed() => if changed.is_err() { break },
                _ = unloading.cancelled() => break,
            }
        }
        self.clear_progress();
    }
    /// The request ended: its progress is gone.
    pub fn clear_progress(&self) {
        self.progress.update(|progress| {
            if progress.request_id.as_deref() == Some(self.scope.request_id.as_str()) {
                *progress = ModelProgress::default();
            }
        });
    }
    /// Visible output arrived; a request replaced by a steer stops here.
    fn receive(&self) -> Result<(), HostError> {
        if self.gate.receive() {
            Ok(())
        } else {
            Err(HostError::Cancelled(REPLACED_BY_STEER.into()))
        }
    }
    /// Commits one output event; the event is checked against before-commit
    /// rewrites.
    fn commit(
        &mut self,
        kind: EventKind,
        id: Option<String>,
    ) -> Result<Arc<AgentEvent>, HostError> {
        let mut draft = EventDraft::new(kind);
        if let Some(id) = id {
            draft = draft.with_id(id);
        }
        let event = self
            .timeline
            .append(draft)
            .map_err(|error| HostError::failed(error.to_string()))?;
        let (turn_id, request_id) = match &event.kind {
            EventKind::Text(d) | EventKind::Thinking(d) => (&d.turn_id, &d.request_id),
            EventKind::ToolUseStart(d) => (&d.turn_id, &d.request_id),
            EventKind::ToolUse(d) => (&d.turn_id, &d.request_id),
            _ => return Ok(event),
        };
        if *turn_id != self.scope.turn_id || *request_id != self.scope.request_id {
            return Err(HostError::failed(
                "Committed output changed request identity",
            ));
        }
        Ok(event)
    }
    fn text(&mut self, delta: String) -> Result<(), HostError> {
        if self.text_part_id.is_none() {
            self.text_part_id = Some(uuid::Uuid::new_v4().to_string());
            self.text_part_committed = false;
        }
        self.thinking_part_id = None;
        self.thinking_part_committed = false;
        let part_id = self.text_part_id.clone();
        self.show(|progress| {
            progress.text_part_id = part_id;
            progress.thinking_part_id = None;
            progress.text.push_str(&delta);
        });
        self.text.push_str(&delta);
        let since = *self.text_since.get_or_insert_with(Instant::now);
        if self.text.len() >= TEXT_FLUSH_BYTES || since.elapsed() >= TEXT_FLUSH_AGE {
            self.flush_text()?;
        }
        Ok(())
    }
    /// Ends the provider's text block. The block identity stays open because
    /// `TextEnd` is not durable: adjacent text events with no thinking or tool
    /// event between them are one block in history.
    fn end_text(&mut self) -> Result<(), HostError> {
        self.flush_text()
    }
    /// Commits the buffered text as one `step/text`.
    pub fn flush_text(&mut self) -> Result<(), HostError> {
        if self.text.is_empty() {
            return Ok(());
        }
        self.text_since = None;
        let delta = std::mem::take(&mut self.text);
        let event_id = if self.text_part_committed {
            None
        } else {
            self.text_part_committed = true;
            self.text_part_id.clone()
        };
        let event = self.commit(
            EventKind::Text(TextData {
                turn_id: self.scope.turn_id.clone(),
                request_id: self.scope.request_id.clone(),
                delta,
            }),
            event_id,
        )?;
        let EventKind::Text(data) = &event.kind else {
            return Err(HostError::failed("Committed output changed its kind"));
        };
        self.committed_text.push_str(&data.delta);
        self.replace_tail(false, &data.delta);
        self.hand_off(event.sequence, Handoff::Text(data.delta.len()));
        Ok(())
    }
    fn thinking_delta(&mut self, thinking: String) -> Result<(), HostError> {
        self.flush_text()?;
        self.text_part_id = None;
        self.text_part_committed = false;
        if self.thinking_part_id.is_none() {
            self.thinking_part_id = Some(uuid::Uuid::new_v4().to_string());
            self.thinking_part_committed = false;
        }
        let part_id = self.thinking_part_id.clone();
        self.show(|progress| {
            progress.text_part_id = None;
            progress.thinking_part_id = part_id;
            progress.thinking.push_str(&thinking);
        });
        Ok(())
    }
    fn thinking(&mut self, thinking: String) -> Result<(), HostError> {
        self.flush_text()?;
        self.text_part_id = None;
        self.text_part_committed = false;
        if thinking.is_empty() {
            self.thinking_part_id = None;
            self.thinking_part_committed = false;
            self.replace_tail(true, "");
            return Ok(());
        }
        if self.thinking_part_id.is_none() {
            self.thinking_part_id = Some(uuid::Uuid::new_v4().to_string());
            self.thinking_part_committed = false;
        }
        let event_id = if self.thinking_part_committed {
            None
        } else {
            self.thinking_part_committed = true;
            self.thinking_part_id.clone()
        };
        let event = self.commit(
            EventKind::Thinking(ThinkingData {
                turn_id: self.scope.turn_id.clone(),
                request_id: self.scope.request_id.clone(),
                delta: thinking,
            }),
            event_id,
        )?;
        let EventKind::Thinking(data) = &event.kind else {
            return Err(HostError::failed("Committed output changed its kind"));
        };
        self.committed_thinking.push_str(&data.delta);
        self.replace_tail(true, &data.delta);
        self.hand_off(event.sequence, Handoff::Thinking(data.delta.len()));
        Ok(())
    }
    fn tool_start(&mut self, call_id: String, name: String) -> Result<(), HostError> {
        self.flush_text()?;
        self.text_part_id = None;
        self.text_part_committed = false;
        self.thinking_part_id = None;
        self.thinking_part_committed = false;
        self.commit(
            EventKind::ToolUseStart(ToolUseStartData {
                turn_id: self.scope.turn_id.clone(),
                request_id: self.scope.request_id.clone(),
                call_id: call_id.clone(),
                name: name.clone(),
            }),
            None,
        )?;
        self.started.push(StartedCall {
            id: call_id.clone(),
            complete: false,
        });
        self.show(|progress| {
            progress.text_part_id = None;
            progress.thinking_part_id = None;
            progress.tools.push(ToolProgress {
                call_id,
                name,
                arguments: String::new(),
            })
        });
        Ok(())
    }
    fn tool_use(&mut self, call: ToolCall) -> Result<(), HostError> {
        self.flush_text()?;
        self.text_part_id = None;
        self.text_part_committed = false;
        self.thinking_part_id = None;
        self.thinking_part_committed = false;
        let event = self.commit(
            EventKind::ToolUse(ToolUseData {
                turn_id: self.scope.turn_id.clone(),
                request_id: self.scope.request_id.clone(),
                call: call.clone(),
            }),
            None,
        )?;
        if let EventKind::ToolUse(data) = &event.kind {
            self.committed_calls
                .insert(data.call.id.clone(), data.call.clone());
        }
        if let Some(started) = self
            .started
            .iter_mut()
            .find(|started| started.id == call.id)
        {
            started.complete = true;
        }
        self.hand_off(event.sequence, Handoff::Tool(call.id));
        Ok(())
    }
    fn content(&mut self, content: Vec<MessagePart>) -> Result<(), HostError> {
        self.flush_text()?;
        let text = content
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        let thinking = content
            .iter()
            .filter_map(|part| match part {
                MessagePart::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect::<String>();
        let mut seen_calls = std::collections::HashSet::new();
        let calls_match = content
            .iter()
            .filter_map(|part| match part {
                MessagePart::ToolUse {
                    id, name, input, ..
                } => Some((id, name, input)),
                _ => None,
            })
            .all(|(id, name, input)| {
                seen_calls.insert(id)
                    && self
                        .committed_calls
                        .get(id)
                        .is_some_and(|call| &call.name == name && &call.input == input)
            });
        if text != self.committed_text
            || thinking != self.committed_thinking
            || !calls_match
            || seen_calls.len() != self.committed_calls.len()
            || content.iter().any(|part| {
                !matches!(
                    part,
                    MessagePart::Text { .. }
                        | MessagePart::Thinking { .. }
                        | MessagePart::ToolUse { .. }
                )
            })
        {
            return Err(HostError::failed(
                "Complete model content differs from committed output",
            ));
        }
        let data = ModelContentData {
            turn_id: self.scope.turn_id.clone(),
            request_id: self.scope.request_id.clone(),
            content,
        };
        let event = self.commit(EventKind::ModelContent(data.clone()), None)?;
        if event.kind != EventKind::ModelContent(data) {
            return Err(HostError::failed(
                "Complete model content was changed before commit",
            ));
        }
        Ok(())
    }
}

pub(crate) async fn consume_output(
    stream: ModelStream,
    output: &mut Output,
    cancel: CancellationToken,
    replay_safe: &mut bool,
) -> Result<FinishReason, HostError> {
    let mut stream = stream.fuse();
    let result = parse_output(&mut stream, output, cancel.clone(), replay_safe).await;
    if result.is_err() {
        cancel.cancel();
        let _ = output.flush_text();
        while stream.next().await.is_some() {}
    }
    result
}

async fn parse_output(
    stream: &mut (impl futures_util::Stream<Item = Result<ModelChunk, HostError>> + Unpin),
    output: &mut Output,
    cancel: CancellationToken,
    replay_safe: &mut bool,
) -> Result<FinishReason, HostError> {
    let mut message_id = None;
    let mut finish = None;
    let mut partial = HashMap::<String, String>::new();
    let mut visible = false;
    let mut content_seen = false;
    let mut published = output.timeline.watch();
    loop {
        let next = tokio::select! {
            chunk = stream.next() => chunk,
            changed = published.changed(), if !output.handoffs.is_empty() => {
                if changed.is_ok() {
                    output.show(|_| {});
                } else {
                    output.handoffs.clear();
                }
                continue;
            }
        };
        let Some(chunk) = next else { break };
        let chunk = match chunk {
            Err(HostError::Failed {
                message,
                budget,
                kind,
                model,
                presentation,
            }) => {
                return Err(HostError::Failed {
                    message,
                    budget: if visible { None } else { budget },
                    kind,
                    model,
                    presentation,
                });
            }
            other => other?,
        };
        if !matches!(chunk, ModelChunk::MessageStart { .. }) {
            *replay_safe = false;
        }
        if let ModelChunk::Usage(usage) = chunk {
            output
                .timeline
                .append(
                    EventKind::ModelUsage(ModelUsageData {
                        turn_id: output.scope.turn_id.clone(),
                        request_id: output.scope.request_id.clone(),
                        usage,
                    })
                    .into(),
                )
                .map_err(|e| HostError::failed(e.to_string()))?;
            continue;
        }
        if finish.is_some() {
            return Err(HostError::failed(
                "Provider emitted output after message_stop",
            ));
        }
        if let ModelChunk::MessageStart { message_id: id } = chunk {
            if message_id.replace(id).is_some() {
                return Err(HostError::failed(
                    "Provider emitted duplicate message_start",
                ));
            }
            continue;
        }
        if message_id.is_none() {
            return Err(HostError::failed("Provider output preceded message_start"));
        }
        if content_seen && !matches!(chunk, ModelChunk::MessageStop { .. }) {
            return Err(HostError::failed(
                "Provider emitted output after complete content",
            ));
        }
        match chunk {
            ModelChunk::Thinking(delta) => {
                output.receive()?;
                visible = true;
                output.thinking_delta(delta)?;
            }
            ModelChunk::ThinkingEnd { thinking } => {
                output.receive()?;
                visible = true;
                output.thinking(thinking)?;
            }
            ModelChunk::Text(delta) => {
                output.receive()?;
                visible = true;
                output.text(delta)?;
            }
            ModelChunk::TextEnd => output.end_text()?,
            ModelChunk::ToolStart { id, name } => {
                output.receive()?;
                visible = true;
                if !output
                    .scope
                    .request
                    .tools
                    .as_deref()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .any(|tool| tool.name == name)
                {
                    return Err(HostError::failed(format!(
                        "Tool was not offered in this request: {name}"
                    )));
                }
                if partial.contains_key(&id) || output.started.iter().any(|call| call.id == id) {
                    return Err(HostError::failed(
                        "Provider changed or reused a tool call identity",
                    ));
                }
                partial.insert(id.clone(), name.clone());
                output.tool_start(id, name)?;
            }
            ModelChunk::ToolArguments { id, name, delta } => {
                output.receive()?;
                visible = true;
                if partial.get(&id) != Some(&name) {
                    return Err(HostError::failed(
                        "Provider sent arguments for a tool call that is not in progress",
                    ));
                }
                output.show(|progress| {
                    if let Some(tool) = progress.tools.iter_mut().find(|tool| tool.call_id == id) {
                        tool.arguments.push_str(&delta);
                    }
                });
            }
            ModelChunk::ToolUse(call) => {
                output.receive()?;
                visible = true;
                if partial.remove(&call.id).as_ref() != Some(&call.name) {
                    return Err(HostError::failed(
                        "Provider completed a tool call that is not in progress",
                    ));
                }
                output.tool_use(call)?;
            }
            ModelChunk::Content(content) => {
                output.receive()?;
                output.content(content)?;
                content_seen = true;
            }
            ModelChunk::MessageStop {
                message_id: id,
                reason,
            } => {
                if message_id.as_ref() != Some(&id) {
                    return Err(HostError::failed("Provider message_stop identity mismatch"));
                }
                if reason == ModelStopReason::MaxTokens {
                    let request = &output.scope.request;
                    let message = format!(
                        "Model {}/{} stopped at max_tokens",
                        request.provider_id, request.model_id
                    );
                    return Err(HostError::Failed {
                        message: message.clone(),
                        budget: None,
                        kind: HostErrorKind::Other,
                        model: None,
                        presentation: Some(Box::new(ProviderPresentation {
                            source: request.provider_id.clone(),
                            reason: "max_tokens".into(),
                            server_message: Some(message),
                            request_id: None,
                            action: Some("Increase the output token limit and retry.".into()),
                        })),
                    });
                }
                if !partial.is_empty() {
                    return Err(HostError::failed("Provider left incomplete tool arguments"));
                }
                if (reason == ModelStopReason::ToolUse) != !output.started.is_empty() {
                    return Err(HostError::failed(
                        "Provider stop reason does not match tool output",
                    ));
                }
                output.flush_text()?;
                finish = Some(match reason {
                    ModelStopReason::EndTurn => FinishReason::EndTurn,
                    ModelStopReason::ToolUse => FinishReason::ToolUse,
                    ModelStopReason::StopSequence => FinishReason::StopSequence,
                    ModelStopReason::MaxTokens => unreachable!(),
                });
            }
            ModelChunk::MessageStart { .. } | ModelChunk::Usage(_) => unreachable!(),
        }
    }
    if let Some(finish) = finish {
        return Ok(finish);
    }
    if cancel.is_cancelled() {
        return Err(HostError::Cancelled("Model invocation cancelled".into()));
    }
    Err(HostError::failed(
        "Provider stream ended without message_stop",
    ))
}

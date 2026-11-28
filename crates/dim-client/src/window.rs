use crate::wire::{Event, EventContent, HistoryWindow};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Default)]
struct ThoughtBoundary {
    turn_end: Option<u64>,
    requests: HashMap<String, ClosedThought>,
}

struct ClosedThought {
    closed_at: u64,
    first: Option<Event>,
    suffix: Vec<String>,
}

impl ClosedThought {
    fn ending(at: u64) -> Self {
        Self {
            closed_at: at,
            first: None,
            suffix: Vec::new(),
        }
    }

    fn prepend(&mut self, event: Event) {
        if let Some(previous) = self.first.replace(event) {
            let EventContent::Thinking(data) = previous.content else {
                unreachable!()
            };
            self.suffix.push(data.delta);
        }
    }

    fn emit(self, rows: &mut Vec<Event>) {
        let Some(mut first) = self.first else { return };
        let EventContent::Thinking(mut data) = first.content else {
            unreachable!()
        };
        for delta in self.suffix.into_iter().rev() {
            data.delta.push_str(&delta);
        }
        first.content = EventContent::SettledThinking(data, self.closed_at);
        rows.push(first);
    }
}

impl ThoughtBoundary {
    fn finish(&mut self, rows: &mut Vec<Event>) {
        for (_, thought) in self.requests.drain() {
            thought.emit(rows);
        }
        self.turn_end = None;
    }

    fn page(&mut self, events: Vec<Event>, at_start: bool) -> Vec<Event> {
        let mut rows = Vec::with_capacity(events.len());
        for event in events.into_iter().rev() {
            let request = match &event.content {
                EventContent::Text(data) => Some(&data.request_id),
                EventContent::ToolUseStart(data) => Some(&data.request_id),
                EventContent::ToolUse(data) => Some(&data.request_id),
                EventContent::ModelEnd(data) => Some(&data.request_id),
                EventContent::ModelStart(data) => Some(&data.request_id),
                EventContent::ToolResult(data) if data.call_event_id.is_some() => {
                    self.finish(&mut rows);
                    None
                }
                EventContent::InputConsumed(_)
                | EventContent::TurnStart(_)
                | EventContent::TurnEnd(_) => {
                    self.finish(&mut rows);
                    if matches!(event.content, EventContent::TurnEnd(_)) {
                        self.turn_end = Some(event.create_at);
                    }
                    None
                }
                EventContent::Thinking(data) => {
                    let thought = match self.turn_end {
                        Some(at) => Some(
                            self.requests
                                .entry(data.request_id.clone())
                                .or_insert_with(|| ClosedThought::ending(at)),
                        ),
                        None => self.requests.get_mut(&data.request_id),
                    };
                    if let Some(thought) = thought {
                        thought.prepend(event);
                        continue;
                    }
                    None
                }
                _ => None,
            };
            if let Some(request) = request {
                if let Some(thought) = self.requests.remove(request) {
                    thought.emit(&mut rows);
                }
                if !matches!(event.content, EventContent::ModelStart(_)) {
                    self.requests
                        .insert(request.clone(), ClosedThought::ending(event.create_at));
                }
            }
            rows.push(event);
        }
        if at_start {
            self.finish(&mut rows);
        }
        rows.sort_unstable_by_key(|event| event.sequence);
        rows
    }
}

/// One bounded transcript load. The consumer takes and releases its raw pages
/// after projecting them; this object is not a history cache.
pub struct Window {
    pub before: Option<u64>,
    pub through: Option<u64>,
    pages: VecDeque<(Option<u64>, Vec<Event>)>,
    input_continuations: Vec<Event>,
    thoughts: ThoughtBoundary,
    initial: bool,
    dropped: bool,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            before: None,
            through: None,
            pages: VecDeque::new(),
            input_continuations: Vec::new(),
            thoughts: ThoughtBoundary::default(),
            initial: true,
            dropped: false,
        }
    }
}

impl Window {
    pub fn earlier(before: u64, through: u64) -> Self {
        Self {
            before: Some(before),
            through: Some(through),
            initial: false,
            ..Self::default()
        }
    }

    pub fn params(&self, session: &str, branch: &str) -> Value {
        self.params_with_tail(session, branch, false)
    }

    pub fn params_with_tail(&self, session: &str, branch: &str, tail: bool) -> Value {
        let mut params =
            json!({"sessionId":session,"branchId":branch,"limit":if self.initial {64} else {256}});
        if let Some(through) = self.through {
            params["through"] = json!(through);
        }
        if let Some(before) = self.before {
            params["before"] = json!(before);
        } else if !tail {
            params["latestTurn"] = json!(true);
        }
        if tail {
            params["tail"] = json!(true);
        }
        params
    }

    /// Selects indexed display kinds when the server advertises transcript pages.
    pub fn params_with_display(
        &self,
        session: &str,
        branch: &str,
        tail: bool,
        transcript: bool,
    ) -> Value {
        let mut params = self.params_with_tail(session, branch, tail || transcript);
        if transcript {
            params["transcript"] = json!(true);
            if self.initial && self.pages.is_empty() {
                params["limit"] = json!(32);
            }
        }
        params
    }

    /// Consumes a wire page; true means this load has reached its budget or
    /// the start of the branch, regardless of the latest-turn `done` flag.
    pub fn push(&mut self, page: HistoryWindow) -> crate::Result<bool> {
        let before = page.next(self.before, self.through)?;
        let events = page
            .events
            .into_iter()
            .map(serde_json::from_value)
            .collect::<serde_json::Result<_>>()?;
        self.before = before;
        self.through = Some(page.through);
        self.pages.push_front((page.next_before, events));
        let mut rows = HashSet::new();
        for event in self.pages.iter().flat_map(|(_, events)| events) {
            rows_of(event, |row| {
                rows.insert(row);
            });
        }
        Ok(self.before.is_none()
            || self.pages.len() >= 4
            || rows.len() >= 100
            || (self.initial && !rows.is_empty()))
    }

    /// Releases whole wire pages in forward order under a 300-identity budget.
    /// The newest page and, when found, the newest page with transcript rows
    /// remain whole even above that budget. This does not read more pages.
    /// The cursor remains at a server-provided position for earlier reads.
    pub fn take(&mut self) -> Vec<Event> {
        self.initial = false;
        let mut rows = HashSet::new();
        let mut keep = 0;
        for (_, events) in self.pages.iter().rev() {
            let previous_rows = rows.len();
            for event in events {
                rows_of(event, |row| {
                    rows.insert(row);
                });
            }
            if previous_rows > 0 && rows.len() > 300 {
                break;
            }
            keep += 1;
        }
        let skip = self.pages.len() - keep;
        self.dropped = skip > 0;
        if skip > 0 {
            self.pages.drain(..skip);
            self.before = self
                .pages
                .front()
                .and_then(|(before, _)| *before)
                .filter(|before| *before > 1);
        }
        std::mem::take(&mut self.pages)
            .into_iter()
            .flat_map(|(_, events)| events)
            .collect()
    }

    /// Events for a separately projected page. Apply and consume facts whose
    /// input was written before the page travel back with the cursor until
    /// the page containing that input can draw it. A settled thought travels
    /// with its close time until its first delta is known. `at_start` is the
    /// indexed transcript's explicit end of history. No extra page is read.
    pub fn take_transcript(&mut self, at_start: bool) -> Vec<Event> {
        let events = self.take();
        let mut events = self
            .thoughts
            .page(events, self.before.is_none() || at_start && !self.dropped);
        events.append(&mut self.input_continuations);
        if self.before.is_some() {
            let written: HashSet<_> = events
                .iter()
                .filter(|event| matches!(event.content, EventContent::Input(_)))
                .map(|event| event.id.clone())
                .collect();
            self.input_continuations
                .extend(events.extract_if(.., |event| match &event.content {
                    EventContent::InputApplied(data) => !written.contains(&data.input_id),
                    EventContent::InputConsumed(data) => !written.contains(&data.input_id),
                    _ => false,
                }));
        }
        events
    }

    /// Whether the last [`Window::take`] dropped the oldest page under the
    /// retention budget: the events it returned then start above the oldest
    /// page read, and `before` asks for the omitted history again.
    pub fn dropped_pages(&self) -> bool {
        self.dropped
    }
}

fn rows_of<'a>(event: &'a Event, mut row: impl FnMut((u8, &'a str, &'a str))) {
    match &event.content {
        EventContent::Input(data) if data.user.is_some() => row((0, &event.id, "")),
        EventContent::Text(data) if !data.delta.is_empty() => row((1, &data.request_id, "")),
        EventContent::ToolUse(data) => row((2, &data.request_id, &data.call.id)),
        EventContent::ToolCompleted(data) => row((2, &data.request_id, &data.call_id)),
        EventContent::ToolResult(data) => {
            for result in &data.results {
                row((2, &data.request_id, &result.call_id));
            }
        }
        EventContent::CompactionStart(_)
        | EventContent::Compaction(_)
        | EventContent::CompactionSummary(_) => row((3, &event.id, "")),
        _ => {}
    }
}

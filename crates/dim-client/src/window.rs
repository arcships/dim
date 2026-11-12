use crate::wire::{Event, EventContent, HistoryWindow};
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};

/// One bounded transcript load. The consumer takes and releases its raw pages
/// after projecting them; this object is not a history cache.
pub struct Window {
    pub before: Option<u64>,
    pub through: Option<u64>,
    pages: VecDeque<(Option<u64>, Vec<Event>)>,
    initial: bool,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            before: None,
            through: None,
            pages: VecDeque::new(),
            initial: true,
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

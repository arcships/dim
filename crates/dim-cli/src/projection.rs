//! Folds App Server session events into the transcript the screen draws.
//!
//! The worker applies every event once, in sequence order, and publishes
//! [`ViewSnapshot`]s built here. Only the rows the screen needs are kept; the
//! durable history stays with the App Server.

use std::collections::HashMap;

use dim_agent_types::{ContentBlock, JsonObject, ModelOutputOutcome, TurnOutcome};
use serde_json::Value;

use crate::model::view::{ApprovalView, RowKind, RowTone, ViewRow, ViewSnapshot};
use crate::wire::{Event, EventContent};

#[derive(Default)]
pub struct Projection {
    rows: Vec<ViewRow>,
    /// The assistant row each model request streams into.
    assistant_rows: HashMap<String, usize>,
    /// The reasoning row each model request streams into, while it is open.
    thinking_rows: HashMap<String, usize>,
    /// The tool row each call id opened.
    tool_rows: HashMap<String, usize>,
    running: bool,
    approvals: Vec<ApprovalView>,
}

impl Projection {
    pub fn apply(&mut self, event: &Event) {
        let sequence = event.sequence;
        match &event.content {
            // Inputs without `user` only carry context blocks, not a message.
            EventContent::Input(data) => {
                if let Some(user) = &data.user {
                    let text = text_of(&user.content);
                    self.push(format!("user-{sequence}"), RowKind::UserMessage, text);
                }
            }
            EventContent::Thinking(data) => {
                let index = match self.thinking_rows.get(&data.request_id) {
                    Some(index) => *index,
                    None => {
                        let index = self.push(
                            format!("thinking-{}", data.request_id),
                            RowKind::Thinking,
                            String::new(),
                        );
                        self.rows[index].streaming = true;
                        self.thinking_rows.insert(data.request_id.clone(), index);
                        index
                    }
                };
                let row = &mut self.rows[index];
                row.text.push_str(&data.delta);
                // While it streams the preview follows the newest line.
                row.preview = row
                    .text
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(|line| line.trim().to_owned());
            }
            EventContent::Text(data) => {
                self.close_thinking(&data.request_id);
                let index = match self.assistant_rows.get(&data.request_id) {
                    Some(index) => *index,
                    None => {
                        let index = self.push(
                            format!("assistant-{}", data.request_id),
                            RowKind::StreamingAssistant,
                            String::new(),
                        );
                        self.rows[index].streaming = true;
                        self.assistant_rows.insert(data.request_id.clone(), index);
                        index
                    }
                };
                self.rows[index].text.push_str(&data.delta);
            }
            EventContent::ToolUse(data) => {
                self.close_thinking(&data.request_id);
                self.close_assistant(&data.request_id);
                let index = self.push(data.call.id.clone(), RowKind::ToolCall, String::new());
                let row = &mut self.rows[index];
                row.tool_name = Some(data.call.name.clone());
                row.tool_input = Some(Value::Object(data.call.input.clone()));
                row.pending = true;
                self.tool_rows.insert(data.call.id.clone(), index);
            }
            EventContent::ToolCompleted(data) => {
                self.complete_tool(&data.call_id, data.result.is_error);
            }
            EventContent::ToolResult(data) => {
                for result in &data.results {
                    self.complete_tool(&result.call_id, result.result.is_error);
                }
            }
            EventContent::ModelEnd(data) => {
                self.close_thinking(&data.request_id);
                self.close_assistant(&data.request_id);
                if let ModelOutputOutcome::Failed { error, .. } = &data.outcome {
                    self.push_error(format!("model failed: {error}"));
                }
            }
            EventContent::TurnEnd(data) => {
                // A turn that ends mid-thought settles what it had.
                let open: Vec<String> = self.thinking_rows.keys().cloned().collect();
                for request_id in open {
                    self.close_thinking(&request_id);
                }
                self.turn_end(sequence, &data.outcome);
            }
            EventContent::Other => {}
        }
    }

    fn turn_end(&mut self, sequence: u64, outcome: &TurnOutcome) {
        match outcome {
            TurnOutcome::Failed { error } => self.push_error(format!("turn failed: {error}")),
            TurnOutcome::Cancelled { .. } => {
                self.push(
                    format!("stopped-{sequence}"),
                    RowKind::Status,
                    "stopped".into(),
                );
            }
            TurnOutcome::Completed { .. } => {}
        }
    }

    pub fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    pub fn add_approval(
        &mut self,
        request_id: String,
        name: String,
        input: JsonObject,
        reason: String,
    ) {
        if let Some(index) = self.tool_rows.get(&request_id).copied() {
            self.rows[index].parked = true;
        }
        self.approvals.push(ApprovalView {
            request_id,
            branch_id: "main".into(),
            reason,
            tool_name: name,
            tool_input: Value::Object(input),
        });
    }

    pub fn remove_approval(&mut self, request_id: &str) {
        self.approvals
            .retain(|approval| approval.request_id != request_id);
    }

    pub fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            rows: self.rows.clone(),
            running: self.running,
            approvals: self.approvals.clone(),
            questions: Vec::new(),
            queued: Vec::new(),
        }
    }

    fn push(&mut self, row_id: String, kind: RowKind, text: String) -> usize {
        self.rows.push(ViewRow {
            row_id,
            kind,
            text,
            streaming: false,
            preview: None,
            tone: None,
            tool_name: None,
            tool_input: None,
            pending: false,
            parked: false,
            result_text: None,
            result_is_error: false,
            diff: None,
        });
        self.rows.len() - 1
    }

    fn push_error(&mut self, text: String) {
        let index = self.push(format!("error-{}", self.rows.len()), RowKind::Error, text);
        self.rows[index].tone = Some(RowTone::Danger);
    }

    /// A settled thought commits as one row: the whole text, whitespace
    /// collapsed (the transcript truncates it to the width).
    fn close_thinking(&mut self, request_id: &str) {
        if let Some(index) = self.thinking_rows.remove(request_id) {
            let row = &mut self.rows[index];
            row.streaming = false;
            row.preview = Some(row.text.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }

    fn close_assistant(&mut self, request_id: &str) {
        if let Some(index) = self.assistant_rows.remove(request_id) {
            self.rows[index].streaming = false;
            self.rows[index].kind = RowKind::AssistantMessage;
        }
    }

    fn complete_tool(&mut self, call_id: &str, is_error: Option<bool>) {
        let Some(index) = self.tool_rows.remove(call_id) else {
            return;
        };
        let row = &mut self.rows[index];
        row.pending = false;
        row.parked = false;
        row.result_is_error = is_error == Some(true);
    }
}

fn text_of(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(projection: &mut Projection, events: &[serde_json::Value]) {
        for event in events {
            let event: Event = serde_json::from_value(event.clone()).unwrap();
            projection.apply(&event);
        }
    }

    fn step(sequence: u64, name: &str, delta: &str) -> serde_json::Value {
        serde_json::json!({
            "sequence": sequence,
            "type": "step",
            "name": name,
            "data": { "turn_id": "t1", "request_id": "r1", "delta": delta },
        })
    }

    #[test]
    fn reasoning_streams_as_one_row_and_settles_before_the_answer() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                // Context-only input: not a user message.
                serde_json::json!({ "sequence": 1, "type": "input", "name": "message", "data": {} }),
                step(2, "thinking", "Look at the\n"),
                step(3, "thinking", "parser first"),
            ],
        );
        let rows = projection.snapshot().rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].kind, RowKind::Thinking);
        assert!(rows[0].streaming);
        assert_eq!(rows[0].preview.as_deref(), Some("parser first"));

        apply(&mut projection, &[step(4, "text", "Done.")]);
        let rows = projection.snapshot().rows;
        assert!(!rows[0].streaming);
        assert_eq!(rows[0].preview.as_deref(), Some("Look at the parser first"));
        assert_eq!(rows[1].kind, RowKind::StreamingAssistant);
    }
}

use crate::Result;
use dim_agent_types::*;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    io::{self, Write},
};

pub fn safe(text: &str) -> String {
    let mut output = String::new();
    let mut escape = 0;
    for ch in text.chars() {
        match escape {
            1 => {
                escape = match ch {
                    '[' => 2,
                    ']' | 'P' | '_' | '^' => 3,
                    _ => 0,
                }
            }
            2 => {
                if ('@'..='~').contains(&ch) {
                    escape = 0;
                }
            }
            3 => {
                if ch == '\u{7}' || ch == '\u{9c}' {
                    escape = 0;
                } else if ch == '\u{1b}' {
                    escape = 4;
                }
            }
            4 => escape = if ch == '\\' { 0 } else { 3 },
            _ => match ch {
                '\u{1b}' => escape = 1,
                '\u{9b}' => escape = 2,
                '\u{9d}' => escape = 3,
                '\n' | '\t' => output.push(ch),
                ch if !ch.is_control()
                    && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
                {
                    output.push(ch)
                }
                _ => {}
            },
        }
    }
    output
}

pub struct Event {
    pub sequence: u64,
    pub content: EventContent,
}
pub enum EventContent {
    Input(InputMessageData),
    TurnEnd(TurnEndData),
    Text(TextData),
    ToolUse(ToolUseData),
    ToolCompleted(Box<ToolCompletedData>),
    ToolResult(Box<ToolResultData>),
    ModelEnd(Box<ModelOutputEndData>),
    Other,
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            sequence: u64,
            r#type: String,
            name: String,
            data: serde_json::Value,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.sequence > JS_MAX_SAFE_INTEGER {
            return Err(serde::de::Error::custom(
                "Event sequence exceeds the protocol range",
            ));
        }
        let content = match (wire.r#type.as_str(), wire.name.as_str()) {
            ("input", "message") => EventContent::Input(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("turn", "end") => EventContent::TurnEnd(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "text") => EventContent::Text(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-use") => EventContent::ToolUse(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-completed") => EventContent::ToolCompleted(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "tool-result") => EventContent::ToolResult(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            ("step", "model-output-end") => EventContent::ModelEnd(
                serde_json::from_value(wire.data).map_err(serde::de::Error::custom)?,
            ),
            _ => EventContent::Other,
        };
        Ok(Self {
            sequence: wire.sequence,
            content,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub session_id: String,
    pub branch_id: String,
    pub request_id: Option<String>,
    pub text: String,
    pub text_offset: u64,
}

#[derive(Default)]
struct TextPosition {
    committed: u64,
    printed: u64,
}

pub struct Display {
    pub json: bool,
    pub cursor: u64,
    pub failed: bool,
    pub muted: bool,
    interactive: bool,
    text_open: bool,
    requests: HashMap<String, TextPosition>,
    tools: HashMap<String, String>,
    finished: HashSet<String>,
    completed_tools: HashSet<String>,
}
impl Display {
    pub fn new(interactive: bool, json: bool) -> Self {
        Self {
            json,
            cursor: 0,
            failed: false,
            muted: false,
            interactive,
            text_open: false,
            requests: HashMap::new(),
            tools: HashMap::new(),
            finished: HashSet::new(),
            completed_tools: HashSet::new(),
        }
    }
    pub fn record(&mut self, message: &str) -> Result<()> {
        if self.muted {
            return Ok(());
        }
        self.finish_text()?;
        if self.interactive {
            println!("{}", safe(message));
        } else {
            eprintln!("{}", safe(message));
        }
        Ok(())
    }
    pub fn shell_progress(&mut self, text: &str) -> Result<()> {
        print!("{}", safe(text));
        self.text_open = !text.ends_with('\n');
        io::stdout().flush()?;
        Ok(())
    }
    pub fn finish_text(&mut self) -> Result<()> {
        if self.text_open {
            println!();
            self.text_open = false;
        }
        io::stdout().flush()?;
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<()> {
        if text.is_empty() || self.muted || self.json {
            return Ok(());
        }
        if !self.text_open && self.interactive {
            println!("Assistant:");
        }
        print!("{}", safe(text));
        io::stdout().flush()?;
        self.text_open = true;
        Ok(())
    }
    pub fn progress(&mut self, progress: Progress) -> Result<()> {
        let Some(request) = progress.request_id else {
            return Ok(());
        };
        if self.finished.contains(&request) {
            return Ok(());
        }
        let position = self.requests.entry(request).or_default();
        // Progress is a replaceable suffix; durable events fill any prefix gap.
        if progress.text_offset > position.printed {
            return Ok(());
        }
        let count = progress.text.chars().count() as u64;
        let skip = position.printed.saturating_sub(progress.text_offset);
        if skip >= count {
            return Ok(());
        }
        position.printed = progress.text_offset + count;
        self.text(
            &progress
                .text
                .chars()
                .skip(skip as usize)
                .collect::<String>(),
        )
    }
    pub fn event(&mut self, event: Event, history: bool) -> Result<()> {
        if event.sequence <= self.cursor {
            return Ok(());
        }
        self.cursor = event.sequence;
        match event.content {
            EventContent::Input(data) if history => {
                if let Some(user) = data.user {
                    let text = user
                        .content
                        .into_iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.record(&format!("> {text}"))?;
                }
            }
            EventContent::Text(data) => {
                let position = self.requests.entry(data.request_id).or_default();
                let count = data.delta.chars().count() as u64;
                let skip = position
                    .printed
                    .saturating_sub(position.committed)
                    .min(count);
                position.committed += count;
                position.printed = position.printed.max(position.committed);
                self.text(&data.delta.chars().skip(skip as usize).collect::<String>())?;
            }
            EventContent::ToolUse(data) => {
                self.tools
                    .insert(data.call.id.clone(), data.call.name.clone());
                self.record(&format!("[tool] {}", data.call.name))?;
            }
            EventContent::ToolCompleted(data) => {
                self.tool_result(data.call_id, data.result.is_error)?;
            }
            EventContent::ToolResult(data) => {
                for result in data.results {
                    self.tool_result(result.call_id, result.result.is_error)?;
                }
            }
            EventContent::ModelEnd(data) => {
                self.finished.insert(data.request_id);
                self.finish_text()?;
                if let ModelOutputOutcome::Failed { .. } = data.outcome {
                    self.failed = true;
                }
            }
            EventContent::TurnEnd(data) => {
                self.finish_text()?;
                match data.outcome {
                    TurnOutcome::Failed { .. } => {
                        self.failed = true;
                        self.record(
                            "[failed] Model or turn failed; see the private daemon log for details",
                        )?;
                    }
                    TurnOutcome::Cancelled { .. } => self.record("[stopped]")?,
                    _ => {}
                }
                self.requests.clear();
            }
            _ => {}
        }
        Ok(())
    }
    fn tool_result(&mut self, call_id: String, is_error: Option<bool>) -> Result<()> {
        if !self.completed_tools.insert(call_id.clone()) {
            return Ok(());
        }
        let name = self.tools.remove(&call_id).unwrap_or(call_id);
        self.record(&format!(
            "[{}] {name}",
            if is_error == Some(true) {
                "failed"
            } else {
                "done"
            }
        ))
    }
}

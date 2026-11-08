use crate::wire::{Configuration, Event, EventContent, Progress};
use crate::{Result, sanitize::safe};
use dim_agent_types::*;
use std::{
    collections::{HashMap, HashSet},
    io::{self, Write},
};

#[derive(Default)]
struct TextPosition {
    committed: u64,
    printed: u64,
}

pub struct Display {
    pub mcp_auth_required: HashSet<String>,
    pub model: Option<Configuration>,
    /// The goal of the branch whose events this display folds.
    pub goal: crate::goal::Fold,
    /// Local sequence of the last `permission/changed` of `main` already
    /// reported, by a record or by the command that wrote it.
    pub permission_seen: u64,
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
    /// Requests that already showed output, for the incomplete prefix.
    showed: HashSet<String>,
    /// The `error` of a model failure already shown this turn: the turn's
    /// repeat of it prints nothing.
    shown_failure: Option<String>,
    /// Whether retry waits are reported; exec keeps them quiet.
    pub retries: bool,
    /// The last attempt a retry line reported, by request id.
    retry_seen: HashMap<String, u8>,
}
impl Display {
    pub fn new(interactive: bool, json: bool) -> Self {
        Self {
            mcp_auth_required: HashSet::new(),
            model: None,
            goal: crate::goal::Fold::default(),
            permission_seen: 0,
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
            showed: HashSet::new(),
            shown_failure: None,
            retries: true,
            retry_seen: HashMap::new(),
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
        if self.retries {
            match &progress.retry {
                Some(retry) if self.retry_seen.get(&request) != Some(&retry.attempt) => {
                    self.retry_seen.insert(request.clone(), retry.attempt);
                    let line = crate::ui::i18n::tf(
                        "retrying {attempt}/{max} in {seconds} s: {reason}",
                        &[
                            ("attempt", &retry.attempt.to_string()),
                            ("max", &retry.max_attempts.to_string()),
                            (
                                "seconds",
                                &crate::projection::retry_seconds(retry.retry_at_ms).to_string(),
                            ),
                            (
                                "reason",
                                &crate::projection::failure_phrase(
                                    retry.kind,
                                    retry.reason.as_deref(),
                                ),
                            ),
                        ],
                    );
                    self.record(&line)?;
                }
                Some(_) => {}
                None => {
                    self.retry_seen.remove(&request);
                }
            }
        }
        if self.finished.contains(&request) {
            return Ok(());
        }
        let position = self.requests.entry(request.clone()).or_default();
        if progress.text_offset > position.printed {
            return Ok(());
        }
        let count = progress.text.chars().count() as u64;
        let skip = position.printed.saturating_sub(progress.text_offset);
        if skip >= count {
            return Ok(());
        }
        position.printed = progress.text_offset + count;
        self.showed.insert(request.clone());
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
        self.goal.apply(&event);
        match event.content {
            EventContent::Configuration(data) => {
                self.model = Some(data);
            }
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
                self.showed.insert(data.request_id.clone());
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
                self.showed.insert(data.request_id.clone());
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
                self.finished.insert(data.request_id.clone());
                let incomplete = self.showed.remove(&data.request_id);
                self.retry_seen.remove(&data.request_id);
                self.finish_text()?;
                if let ModelOutputOutcome::Failed {
                    error,
                    presentation,
                    failure,
                    ..
                } = data.outcome
                {
                    self.failed = true;
                    let line = match (presentation, failure) {
                        (Some(presentation), _) => Some(format!(
                            "[failed] {}: {}{}",
                            presentation.source,
                            presentation
                                .server_message
                                .as_deref()
                                .unwrap_or(&presentation.reason),
                            presentation
                                .action
                                .map(|action| format!(". {action}"))
                                .unwrap_or_default(),
                        )),
                        (None, Some(failure)) => Some(format!(
                            "[failed] {}",
                            crate::projection::failure_line(&failure, incomplete)
                        )),
                        (None, None) => None,
                    };
                    if let Some(line) = line {
                        self.record(&line)?;
                        self.shown_failure = Some(error);
                    }
                }
            }
            EventContent::TurnEnd(data) => {
                self.finish_text()?;
                let presented = self.shown_failure.take();
                match data.outcome {
                    TurnOutcome::Failed { error } => {
                        self.failed = true;
                        if presented.as_deref() != Some(error.as_str()) {
                            self.record(
                                "[failed] Model or turn failed; see the private daemon log for details",
                            )?;
                        }
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

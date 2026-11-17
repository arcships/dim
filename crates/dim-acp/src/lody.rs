//! Lody Core 0.2.0 boundary controls and snapshot-only delegation projection.
use crate::projection::tool_id;
use agent_client_protocol::Error;
use dim_agent_types::{CollaborationDelivery, CollaborationOutcome, PermissionPreset};
use dim_client::wire::{Event, EventContent};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Controls {
    pub startup: Option<String>,
    pub mode: Option<PermissionPreset>,
    pub fork_turn: Option<String>,
}
pub(crate) fn controls(method: &str, value: &Value) -> Result<Controls, Error> {
    let Some(lody) = value.get("_meta").and_then(|meta| meta.get("lody")) else {
        return Ok(Controls::default());
    };
    let meta = lody.as_object().ok_or_else(Error::invalid_params)?;
    for field in ["steer", "goalControl", "worktreeProject"] {
        if meta.contains_key(field) {
            return Err(unsupported(field));
        }
    }
    let mut controls = Controls::default();
    if let Some(value) = meta.get("sessionConfig") {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Startup {
            version: u32,
            model_id: Option<String>,
            config_option_values: HashMap<String, OptionValue>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OptionValue {
            Text(String),
            Boolean(bool),
        }
        let startup: Startup =
            serde_json::from_value(value.clone()).map_err(|_| Error::invalid_params())?;
        if startup.version != 1 {
            return Err(Error::invalid_params());
        }
        if !matches!(
            method,
            "session/new" | "session/load" | "session/resume" | "session/fork"
        ) {
            return Err(unsupported("sessionConfig"));
        }
        if let Some(key) = startup
            .config_option_values
            .keys()
            .find(|key| !matches!(key.as_str(), "model" | "mode"))
        {
            return Err(unsupported(&format!(
                "sessionConfig.configOptionValues.{key}"
            )));
        }
        controls.startup = match startup.model_id {
            Some(model) => Some(model),
            None => match startup.config_option_values.get("model") {
                Some(OptionValue::Text(model)) => Some(model.clone()),
                Some(OptionValue::Boolean(true | false)) => {
                    return Err(Error::invalid_params());
                }
                None => None,
            },
        };
        if controls.startup.as_ref().is_some_and(String::is_empty) {
            return Err(Error::invalid_params());
        }
        controls.mode = match startup.config_option_values.get("mode") {
            Some(OptionValue::Text(mode)) => {
                Some(serde_json::from_value(json!(mode)).map_err(|_| Error::invalid_params())?)
            }
            Some(OptionValue::Boolean(_)) => return Err(Error::invalid_params()),
            None => None,
        };
        if method != "session/new" && (controls.startup.is_some() || controls.mode.is_some()) {
            return Err(unsupported("sessionConfig"));
        }
    }
    if let Some(value) = meta.get("forkAtTurn") {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Anchor {
            version: u32,
            turn_id: Option<String>,
        }
        let anchor: Anchor =
            serde_json::from_value(value.clone()).map_err(|_| Error::invalid_params())?;
        if anchor.version != 1 || anchor.turn_id.as_ref().is_some_and(String::is_empty) {
            return Err(Error::invalid_params());
        }
        if method != "session/fork" {
            return Err(unsupported("forkAtTurn"));
        }
        controls.fork_turn = anchor.turn_id;
    }
    Ok(controls)
}
fn unsupported(field: &str) -> Error {
    Error::new(-32602, format!("Unsupported Lody control: {field}"))
        .data(json!({"code":"unsupported_control","field":field}))
}

pub(crate) fn capabilities(fork: bool) -> Value {
    let mut value = json!({"sessionHistory":{"version":2},"compaction":{"version":1},"tasks":{"version":1,"background":true},"subagentEvents":{"version":1}});
    if fork {
        value["forkAtTurn"] = json!({"version":1});
    }
    value
}

struct Run {
    id: String,
    parent_tool: String,
    description: String,
    started: f64,
    state: &'static str,
    ended: Option<f64>,
    summary: Option<String>,
    reason: Option<(&'static str, String)>,
    incomplete: bool,
}
impl Run {
    fn terminal(&self) -> bool {
        !matches!(self.state, "pending" | "running")
    }
    fn event(&self, session: &str) -> Value {
        let mut snapshot = json!({"state":self.state,"parentToolCallId":self.parent_tool,"description":self.description,"startedAtEpochSeconds":self.started,"support":{"stream":[],"progress":false,"outputRead":"none","cancel":false}});
        if let Some(ended) = self.ended {
            snapshot["endedAtEpochSeconds"] = json!(ended);
        }
        if let Some(summary) = &self.summary {
            snapshot["summary"] = json!(summary);
        }
        if let Some((code, message)) = &self.reason {
            snapshot["reason"] = json!({"code":code,"message":message});
        }
        if self.incomplete {
            snapshot["outputIncomplete"] = json!(true);
        }
        json!({"version":1,"sessionId":session,"runId":self.id,"type":"snapshot","snapshot":snapshot})
    }
}
pub(crate) struct Subagents {
    activation: String,
    runs: HashMap<String, Run>,
    through: u64,
}
impl Default for Subagents {
    fn default() -> Self {
        Self {
            activation: uuid::Uuid::new_v4().to_string(),
            runs: HashMap::new(),
            through: 0,
        }
    }
}
impl Subagents {
    pub(crate) fn pending(
        &mut self,
        session: &str,
        request: &dim_agent_types::PendingDelegation,
    ) -> Value {
        let run = Run {
            id: json!([self.activation, request.event_id]).to_string(),
            parent_tool: tool_id(
                "main",
                &request.request.request_id,
                &request.request.call_id,
            ),
            description: request.request.prompt.chars().take(4096).collect(),
            started: request.started_at as f64 / 1000.,
            state: if request.delivery.is_some() {
                "running"
            } else {
                "pending"
            },
            ended: None,
            summary: None,
            reason: None,
            incomplete: false,
        };
        let snapshot = run.event(session);
        self.runs.insert(request.event_id.clone(), run);
        snapshot
    }

    pub(crate) fn event(&mut self, session: &str, event: &Event) -> Vec<Value> {
        let mut output = Vec::new();
        if event.sequence <= self.through {
            return output;
        }
        self.through = event.sequence;
        match &event.content {
            EventContent::CollaborationRequest(data) => {
                if self.runs.contains_key(&event.id) {
                    return output;
                }
                let run = Run {
                    id: json!([self.activation, event.id]).to_string(),
                    parent_tool: tool_id("main", &data.request_id, &data.call_id),
                    description: data.prompt.chars().take(4096).collect(),
                    started: event.create_at as f64 / 1000.,
                    state: "pending",
                    ended: None,
                    summary: None,
                    reason: None,
                    incomplete: false,
                };
                output.push(run.event(session));
                self.runs.insert(event.id.clone(), run);
            }
            EventContent::CollaborationDelivered(data) => {
                if let Some(run) = self.runs.get_mut(&data.op_event_id)
                    && !run.terminal()
                {
                    match &data.delivery {
                        CollaborationDelivery::Ok { .. } => run.state = "running",
                        CollaborationDelivery::Error { .. } => {
                            run.state = "failed";
                            run.ended = Some(event.create_at as f64 / 1000.);
                            run.reason = Some(("error", "Delegation delivery failed".into()));
                        }
                    }
                    output.push(run.event(session));
                }
            }
            EventContent::CollaborationResult(data) => {
                if let Some(run) = self.runs.get_mut(&data.request_event_id)
                    && !run.terminal()
                {
                    match &data.outcome {
                        CollaborationOutcome::Completed => run.state = "completed",
                        CollaborationOutcome::Failed { reason } => {
                            run.state = "failed";
                            run.reason = Some(("error", reason.clone()));
                        }
                        CollaborationOutcome::Cancelled { reason } => {
                            run.state = "cancelled";
                            run.reason = Some(("cancelled", reason.clone()));
                        }
                    }
                    run.ended = Some(event.create_at as f64 / 1000.);
                    run.summary = Some(data.text.chars().take(4096).collect());
                    output.push(run.event(session));
                }
            }
            _ => {}
        }
        output
    }
    pub(crate) fn disconnected(&mut self, session: &str) -> Vec<Value> {
        let mut output = Vec::new();
        for run in self.runs.values_mut().filter(|run| !run.terminal()) {
            run.state = "unknown";
            run.incomplete = true;
            run.reason = Some(("disconnected", "Native observation was interrupted".into()));
            output.push(run.event(session));
        }
        *self = Self::default();
        output
    }
}

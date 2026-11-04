//! Workflow runs of the displayed Session: one status read at a time, through
//! the advertised `workflow.*` management methods.

use std::collections::{BTreeMap, HashSet};

use super::*;
use crate::wire::{WorkflowRun, WorkflowState, workflow_report, workflow_tool_run};

/// The least time between automatic reads of known unfinished runs.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct WorkflowReads {
    /// `workflow` tool calls whose results can name a run.
    calls: HashSet<(String, String, String)>,
    /// Run IDs the Session's facts named, in rotation order.
    known: BTreeMap<String, Known>,
    /// The run the last automatic read named.
    last: Option<String>,
    /// An explicit discovery waits for the read slot.
    discovery: bool,
    /// Outlives a replaced attachment until its reply arrives.
    pub request: Option<StatusRead>,
    /// When the next automatic read may go out.
    pub next_at: Option<tokio::time::Instant>,
    /// Automatic reads stop after a failure until the panel reopens.
    error: Option<String>,
    observed: bool,
    pub stops: Vec<WorkflowStop>,
}

#[derive(Default)]
struct Known {
    /// A fact reported the run's end.
    ended: bool,
    /// The read after that end went out.
    settled: bool,
    /// The owner no longer returns the run.
    unavailable: bool,
}

pub(super) struct StatusRead {
    pub id: u64,
    projection_id: u64,
    /// `None` for an explicit discovery.
    run_id: Option<String>,
    /// A stop reply settled after this read was sent.
    stale: bool,
}

pub(super) struct WorkflowStop {
    pub id: u64,
    run_id: String,
    projection_id: u64,
}

impl WorkflowReads {
    pub fn inventory(&self, supported: bool) -> WorkflowInventory {
        if !supported {
            WorkflowInventory::Absent
        } else if let Some(error) = &self.error {
            WorkflowInventory::Failed {
                error: error.clone(),
                observed: self.observed,
            }
        } else if self.discovery
            || self
                .request
                .as_ref()
                .is_some_and(|request| request.run_id.is_none() && !request.stale)
        {
            WorkflowInventory::Loading
        } else {
            WorkflowInventory::Ready
        }
    }

    /// A fresh attachment knows no runs; an outstanding read keeps its slot.
    pub fn clear(&mut self) {
        *self = Self {
            request: self.request.take(),
            stops: std::mem::take(&mut self.stops),
            ..Self::default()
        };
    }
}

impl Worker {
    fn workflow_supported(&self) -> bool {
        self.client.supports(&["workflow.status"])
    }

    /// Finds run IDs in the producer-defined fields of tool results and
    /// report inputs, never in result prose.
    pub(super) fn discover_workflow(&mut self, branch_id: &str, event: &Event) {
        let mut observed = Vec::new();
        match &event.content {
            EventContent::ToolCall(data) => {
                for call in data.calls.iter().filter(|call| call.name == "workflow") {
                    self.workflow.calls.insert((
                        branch_id.to_owned(),
                        data.request_id.clone(),
                        call.id.clone(),
                    ));
                }
            }
            EventContent::ToolCompleted(data) => {
                if self.workflow.calls.remove(&(
                    branch_id.to_owned(),
                    data.request_id.clone(),
                    data.call_id.clone(),
                )) {
                    observed.extend(workflow_tool_run(&data.result));
                }
            }
            EventContent::ToolResult(data) => {
                for result in &data.results {
                    if self.workflow.calls.remove(&(
                        branch_id.to_owned(),
                        data.request_id.clone(),
                        result.call_id.clone(),
                    )) {
                        observed.extend(workflow_tool_run(&result.result));
                    }
                }
            }
            EventContent::Input(data) => {
                for block in &data.blocks {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    observed.extend(workflow_report(text));
                }
            }
            _ => {}
        }
        for (run_id, state) in observed {
            let known = self.workflow.known.entry(run_id).or_default();
            known.ended |= !state.unfinished();
            self.workflow
                .next_at
                .get_or_insert_with(tokio::time::Instant::now);
        }
    }

    /// Reads a known run while the owner may still change it: an unfinished
    /// one, or once more after a fact reported its end.
    fn workflow_refreshes(&self, run_id: &str, known: &Known) -> bool {
        let record = self.projection.workflow(run_id);
        !known.unavailable
            && if known.ended {
                !known.settled && record.is_some_and(|view| view.run.state.unfinished())
            } else {
                record.is_none_or(|view| view.run.state.unfinished())
            }
    }

    /// Reopening the panel asks for the retained inventory and resumes reads
    /// a failure stopped.
    pub(super) fn discover_workflows(&mut self) {
        if self.workflow_supported() {
            self.workflow.discovery = true;
            self.workflow.error = None;
            self.changed();
        }
    }

    pub(super) async fn poll_workflow(&mut self) {
        if !self.workflow_supported()
            || self.session_id.is_empty()
            || self.workflow.request.is_some()
            || self.workflow.error.is_some()
        {
            return;
        }
        let run_id = if self.workflow.discovery {
            None
        } else {
            if self
                .workflow
                .next_at
                .is_none_or(|at| at > tokio::time::Instant::now())
            {
                return;
            }
            let last = self.workflow.last.clone().unwrap_or_default();
            let refreshes = |(id, known): &(&String, &Known)| self.workflow_refreshes(id, known);
            let next = self
                .workflow
                .known
                .iter()
                .filter(|(id, _)| **id > last)
                .find(refreshes)
                .or_else(|| self.workflow.known.iter().find(refreshes))
                .map(|(id, _)| id.clone());
            self.workflow.next_at = None;
            let Some(id) = next else { return };
            let known = self.workflow.known.get_mut(&id).expect("known run");
            known.settled = known.ended;
            self.workflow.last = Some(id.clone());
            Some(id)
        };
        let params = match &run_id {
            Some(id) => json!({"run_id": id}),
            None => json!({}),
        };
        match self.client.send("workflow.status", params).await {
            Ok(id) => {
                self.workflow.discovery &= run_id.is_some();
                self.workflow.request = Some(StatusRead {
                    id,
                    projection_id: self.projection_id,
                    run_id,
                    stale: false,
                });
            }
            Err(error) => self.workflow.error = Some(error.to_string()),
        }
        self.changed();
    }

    pub(super) fn workflow_reply(&mut self, result: Result<serde_json::Value, client::RpcError>) {
        let request = self.workflow.request.take().expect("matched status read");
        if request.projection_id != self.projection_id {
            return;
        }
        self.workflow.next_at = Some(tokio::time::Instant::now() + REFRESH_INTERVAL);
        self.changed();
        if request.stale {
            self.workflow.discovery |= request.run_id.is_none();
            return;
        }
        let session_id = self.session_id.clone();
        match (request.run_id, result) {
            (Some(run_id), Err(error)) if app_error(&error) == Some("run_not_found") => {
                self.workflow
                    .known
                    .entry(run_id.clone())
                    .or_default()
                    .unavailable = true;
                self.projection.workflow_unavailable(&run_id);
            }
            (_, Err(error)) => self.workflow.error = Some(error.to_string()),
            (None, Ok(value)) => match serde_json::from_value::<Vec<WorkflowRun>>(value) {
                Ok(runs) => {
                    let runs: Vec<_> = runs
                        .into_iter()
                        .filter_map(|run| Some((run.branch_of(&session_id)?.to_owned(), run)))
                        .collect();
                    for (_, run) in &runs {
                        self.workflow.known.entry(run.run_id.clone()).or_default();
                    }
                    self.workflow.observed = true;
                    self.projection.reconcile_workflows(runs);
                }
                Err(_) => self.workflow.error = Some("Invalid workflow.status response".into()),
            },
            (Some(run_id), Ok(value)) => match serde_json::from_value::<WorkflowRun>(value) {
                Ok(run) if run.run_id == run_id => {
                    self.workflow.observed = true;
                    match run.branch_of(&session_id).map(str::to_owned) {
                        Some(branch_id) => self.projection.set_workflow(&branch_id, run),
                        None => {
                            self.workflow
                                .known
                                .entry(run_id.clone())
                                .or_default()
                                .unavailable = true;
                            self.projection.workflow_unavailable(&run_id);
                        }
                    }
                }
                _ => self.workflow.error = Some("Invalid workflow.status response".into()),
            },
        }
    }

    /// Single-run stop from the panel; Workflow settles its stages.
    pub(super) async fn stop_workflow(
        &mut self,
        session_id: String,
        projection_id: u64,
        run_id: String,
    ) {
        if session_id != self.session_id || projection_id != self.projection_id {
            self.send(Update::Notice(
                "Task attachment changed; reopen the panel before stopping work".into(),
            ));
            return;
        }
        if !self.client.supports(&["workflow.stop"]) {
            self.send(Update::Notice("Workflow stop unavailable".into()));
            return;
        }
        if !self
            .projection
            .workflow(&run_id)
            .is_some_and(|view| view.run.state == WorkflowState::Running && !view.stop_pending)
        {
            return;
        }
        match self
            .client
            .send("workflow.stop", json!({"run_id": run_id}))
            .await
        {
            Ok(id) => {
                self.projection.workflow_stop_pending(&run_id, true);
                self.workflow.stops.push(WorkflowStop {
                    id,
                    run_id,
                    projection_id,
                });
            }
            Err(error) => self.send(Update::Notice(format!("Workflow stop {run_id}: {error}"))),
        }
        self.changed();
    }

    pub(super) fn workflow_stop_reply(
        &mut self,
        index: usize,
        result: Result<serde_json::Value, client::RpcError>,
    ) {
        let stop = self.workflow.stops.remove(index);
        if stop.projection_id != self.projection_id {
            return;
        }
        if let Err(error) = self.workflow_stopped(&stop.run_id, result) {
            self.send(Update::Notice(format!(
                "Workflow stop {}: {error}",
                stop.run_id
            )));
        }
    }

    /// Adopts a stop reply's stored run over any read sent before it.
    pub(super) fn workflow_stopped(
        &mut self,
        run_id: &str,
        result: Result<serde_json::Value, client::RpcError>,
    ) -> Result<(), String> {
        self.projection.workflow_stop_pending(run_id, false);
        self.changed();
        let value = result.map_err(|error| error.to_string())?;
        let run = serde_json::from_value::<WorkflowRun>(value)
            .ok()
            .filter(|run| run.run_id == run_id)
            .ok_or("Invalid workflow.stop response")?;
        if let Some(request) = &mut self.workflow.request {
            request.stale = true;
        }
        if let Some(branch_id) = run.branch_of(&self.session_id).map(str::to_owned) {
            self.projection.set_workflow(&branch_id, run);
        }
        Ok(())
    }

    /// Stop-all cannot describe Workflow runs while their inventory is
    /// loading or failed.
    pub(super) fn workflow_incomplete(&self) -> bool {
        matches!(
            self.workflow.inventory(self.workflow_supported()),
            WorkflowInventory::Loading | WorkflowInventory::Failed { .. }
        )
    }
}

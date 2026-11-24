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

    /// A new run fact asks for a read, unless a failure stopped automatic
    /// reads until the panel reopens.
    pub fn schedule(&mut self) {
        if self.error.is_none() {
            self.next_at.get_or_insert_with(tokio::time::Instant::now);
        }
    }

    /// A failed read stops automatic reads; no wake stays scheduled.
    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.next_at = None;
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

    /// A status read may start now: the connection advertises the method, the
    /// Session is attached, no read is in flight and no failure stopped reads.
    pub(super) fn workflow_ready(&self) -> bool {
        self.workflow_supported()
            && !self.session_id.is_empty()
            && self.workflow.request.is_none()
            && self.workflow.error.is_none()
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
            self.workflow.schedule();
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
        if !self.workflow_ready() {
            // A read in flight is a busy slot, not a stopped reader: a
            // replacement attachment's deadline waits for it and the timer
            // shares this eligibility check, so the busy slot cannot spin it.
            if self.workflow.request.is_none() {
                self.workflow.next_at = None;
            }
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
            Err(error) => self.workflow.fail(error.to_string()),
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
            (_, Err(error)) => self.workflow.fail(error.to_string()),
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
                Err(_) => self
                    .workflow
                    .fail("Invalid workflow.status response".into()),
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
                _ => self
                    .workflow
                    .fail("Invalid workflow.status response".into()),
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::Message;

    fn step(sequence: u64, name: &str, data: serde_json::Value) -> Event {
        serde_json::from_value(json!({
            "id": format!("{name}-{sequence}"),
            "sequence": sequence,
            "create_at": 2000,
            "type": "step",
            "name": name,
            "data": data,
        }))
        .expect("a step event")
    }

    fn call(sequence: u64, id: &str) -> Event {
        step(
            sequence,
            "tool-call",
            json!({
                "turn_id": "turn",
                "request_id": "request",
                "calls": [{"id": id, "name": "workflow", "input": {"action": "start", "name": "nightly-check"}}],
            }),
        )
    }

    fn completed(sequence: u64, id: &str, run_id: &str) -> Event {
        step(
            sequence,
            "tool-completed",
            json!({
                "turn_id": "turn",
                "request_id": "request",
                "call_id": id,
                "call_event_id": format!("{id}-use"),
                "result": {
                    "content": format!(r#"{{"run_id":"{run_id}"}}"#),
                    "structuredContent": {"run_id": run_id, "status": "running"},
                },
            }),
        )
    }

    /// Answers the outstanding status read with `result`.
    async fn reply(worker: &mut Worker, result: Result<serde_json::Value, client::RpcError>) {
        let id = worker
            .workflow
            .request
            .as_ref()
            .expect("a status read in flight")
            .id;
        worker
            .message(Message::Response { id, result })
            .await
            .unwrap();
    }

    /// A failed status read stops automatic reads: it keeps no wake for the
    /// Session loop, a run fact that arrives while they are stopped does not
    /// schedule one, and reopening the panel reads again. An RPC failure, a
    /// discovery reply that is not a run list and a run record that does not
    /// decode all end the same way; a successful reply schedules the next read.
    #[tokio::test]
    async fn a_failed_workflow_read_keeps_no_wake_for_the_next_read() {
        let mut features = crate::session_host::TEST_FEATURES.to_vec();
        features.push("workflow.status");
        let (mut worker, _receiver, _sent) = crate::session_host::test_worker(&features).await;
        worker.session_id = "s1".into();
        worker.discover_workflow("main", &call(1, "wf-call"));
        worker.discover_workflow("main", &completed(2, "wf-call", "run-a"));
        assert!(
            worker.workflow.next_at.is_some(),
            "the run fact asks for a status read"
        );

        worker.poll_workflow().await;
        reply(
            &mut worker,
            Err(client::RpcError {
                code: -32000,
                message: "store busy".into(),
                data: Some(json!({"appError": "service_unavailable"})),
            }),
        )
        .await;
        assert!(worker.workflow.error.is_some(), "the failure is recorded");
        assert!(
            worker.workflow.next_at.is_none(),
            "a failed read leaves no automatic read wake"
        );

        worker.discover_workflow("main", &call(3, "wf-call-2"));
        worker.discover_workflow("main", &completed(4, "wf-call-2", "run-b"));
        assert!(
            worker.workflow.next_at.is_none(),
            "a stopped reader schedules no read"
        );
        worker.poll_workflow().await;
        assert!(
            worker.workflow.request.is_none(),
            "a stopped reader sends no read"
        );

        worker.discover_workflows();
        worker.poll_workflow().await;
        assert!(
            worker.workflow.request.is_some(),
            "reopening the panel reads again"
        );
        reply(&mut worker, Ok(json!({"not": "a run list"}))).await;
        assert!(
            worker.workflow.next_at.is_none(),
            "a discovery reply that is not a run list leaves no wake"
        );

        worker.discover_workflows();
        worker.poll_workflow().await;
        reply(&mut worker, Ok(json!([]))).await;
        assert!(
            worker.workflow.next_at.is_some(),
            "a successful reply schedules the next read"
        );

        worker.workflow.next_at = Some(tokio::time::Instant::now());
        worker.poll_workflow().await;
        reply(&mut worker, Ok(json!({"run_id": "run-a"}))).await;
        assert!(
            worker.workflow.next_at.is_none(),
            "a run record that does not decode leaves no wake"
        );
    }

    /// A replayed run fact asks for a status read even when the connection
    /// does not advertise `workflow.status`; no read can follow, so the poll
    /// drops the deadline instead of leaving the Session loop an expired one.
    #[tokio::test]
    async fn a_replayed_run_fact_without_the_workflow_method_keeps_no_wake() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        assert!(
            !worker.client.supports(&["workflow.status"]),
            "the test daemon advertises no workflow status"
        );
        worker.session_id = "s1".into();
        worker.replay("main", call(1, "wf-call"));
        worker.replay("main", completed(2, "wf-call", "run-a"));
        assert!(
            worker.workflow.next_at.is_some(),
            "the replayed run fact asks for a status read"
        );

        worker.poll_workflow().await;
        assert!(
            worker.workflow.request.is_none(),
            "a connection without workflow.status sends no read"
        );
        assert!(
            worker.workflow.next_at.is_none(),
            "a connection without workflow.status leaves no wake armed"
        );
    }

    /// A replacement attachment relays its run facts while the old attachment's
    /// read is still in flight. That reply frees the slot, and the deadline the
    /// replay armed reads the replayed run without reopening the panel.
    #[tokio::test]
    async fn a_replaced_attachments_replayed_run_is_read_once_the_old_reply_lands() {
        let mut features = crate::session_host::TEST_FEATURES.to_vec();
        features.push("workflow.status");
        let (mut worker, _receiver, _sent) = crate::session_host::test_worker(&features).await;
        worker.session_id = "s1".into();
        worker.discover_workflow("main", &call(1, "wf-call"));
        worker.discover_workflow("main", &completed(2, "wf-call", "run-a"));
        worker.poll_workflow().await;
        let old = worker
            .workflow
            .request
            .as_ref()
            .expect("the old attachment's read is in flight")
            .id;

        worker.session_id = worker.invalidate_projection();
        assert!(
            worker.workflow.next_at.is_none(),
            "a replacement starts with no deadline"
        );
        worker.replay("main", call(3, "wf-call-2"));
        worker.replay("main", completed(4, "wf-call-2", "run-b"));
        assert!(
            worker.workflow.next_at.is_some(),
            "the replayed run asks for a read"
        );

        worker.poll_workflow().await;
        assert_eq!(
            worker.workflow.request.as_ref().map(|request| request.id),
            Some(old),
            "the replayed run waits for the read in flight"
        );
        assert!(
            worker.workflow.next_at.is_some(),
            "a busy read keeps the replacement's deadline"
        );

        reply(&mut worker, Ok(json!({"run_id": "run-a"}))).await;
        assert!(
            worker.workflow.request.is_none(),
            "the old reply frees the slot"
        );
        assert!(
            worker.workflow.next_at.is_some(),
            "the old reply leaves the replacement's deadline armed"
        );

        worker.poll_workflow().await;
        assert_eq!(
            worker
                .workflow
                .request
                .as_ref()
                .and_then(|request| request.run_id.as_deref()),
            Some("run-b"),
            "the replacement's replayed run is read without reopening the panel"
        );
    }
}

//! Finite history pages and addressed stops owned by the Session worker.

use std::collections::{HashSet, VecDeque};

use dim_agent_types::BranchCreateFailedData;

use super::*;
use crate::model::view::{MemberTarget, TaskKind};

#[derive(Default)]
pub(super) struct BranchHistory {
    window: dim_client::window::Window,
    pub done: bool,
    pub error: Option<String>,
    live: Vec<Event>,
    window_complete: bool,
    inventory_index: usize,
    inventory_cursor: Option<serde_json::Value>,
    goal_stale: bool,
    earlier: EarlierRead,
    pub running: Option<bool>,
    pub latest_reply: u64,
    pub seen_reply: u64,
}

/// The earlier read a branch is recorded as waiting for: none, one queued or
/// in flight, or one that failed while the window still holds the older
/// request it was asked for. Only `Reading` folds the page that lands as older
/// history; `Failed` is there to be read again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EarlierRead {
    #[default]
    None,
    Reading,
    Failed,
}

pub(super) struct HistoryRequest {
    pub id: u64,
    session_id: String,
    branch_id: String,
    current: bool,
    stage: ReadStage,
}

enum ReadStage {
    History,
    Cut,
    Inventory(&'static str),
    Goal,
    ToolOrigin { request_id: String, call_id: String },
}

pub(super) struct StopBatch {
    session_id: String,
    projection_id: u64,
    targets: VecDeque<TaskKind>,
    /// The outstanding request, its label and the Workflow run it stops.
    pub request: Option<(u64, String, Option<String>)>,
    attempted: usize,
    errors: usize,
    unavailable: usize,
}

impl Worker {
    /// The replacement releases this attachment after creating or finding its Session.
    pub(super) fn invalidate_projection(&mut self) -> String {
        self.compaction_targets(Default::default());
        self.release_members();
        self.projection_id += 1;
        let mut attached_session = std::mem::take(&mut self.session_id);
        if attached_session.is_empty() {
            attached_session = std::mem::take(&mut self.parked);
        }
        self.projection = Projection::default();
        self.session_cwd = None;
        self.send(Update::Skills(Vec::new()));
        self.session_title = None;
        self.title_revision = 0;
        self.last_usage = None;
        self.context_request = None;
        self.context_stale = false;
        self.cursors.clear();
        self.focus = "main".into();
        self.side = None;
        self.lines.clear();
        self.task_history.clear();
        self.task_history_queue.clear();
        self.workflow.clear();
        if let Some(request) = &mut self.task_history_request {
            request.current = false;
        }
        self.watch_task_output(None);
        self.publish();
        attached_session
    }

    pub(super) fn discover_branch(&mut self, branch_id: &str) {
        if !self.task_history.contains_key(branch_id) {
            self.task_history
                .insert(branch_id.to_owned(), BranchHistory::default());
            self.task_history_queue.push_back(branch_id.to_owned());
            self.changed();
        }
    }

    pub(super) fn line_projection(&mut self, branch_id: &str) -> &mut LineProjection {
        let history = self
            .task_history
            .get_mut(branch_id)
            .expect("discovered line");
        history.seen_reply = history.latest_reply;
        if !self.lines.contains_key(branch_id)
            && (history.window.through.is_some() || history.done || history.error.is_some())
        {
            history.window = Default::default();
            history.done = false;
            history.error = None;
            history.window_complete = false;
            history.inventory_index = 0;
            history.inventory_cursor = None;
            history.earlier = EarlierRead::None;
            if let Some(request) = &mut self.task_history_request
                && request.branch_id == branch_id
            {
                request.current = false;
            }
            if !self.task_history_queue.iter().any(|id| id == branch_id) {
                self.task_history_queue.push_back(branch_id.to_owned());
            }
        }
        let running = self.task_history.get(branch_id).and_then(|at| at.running);
        let line = self
            .lines
            .entry(branch_id.to_owned())
            .or_insert_with(|| LineProjection::new(branch_id));
        if let Some(running) = running {
            line.projection.set_running(running);
        }
        line
    }

    pub(super) fn discover_task_branches(&mut self, event: &Event) {
        match &event.content {
            EventContent::BranchCreate(data) => self.discover_branch(&data.branch),
            EventContent::BranchStart(data) => {
                if let Some(parent) = &data.parent {
                    self.discover_branch(parent);
                }
            }
            EventContent::BranchCreateFailed(data) => {
                let (branch, error) = match data {
                    BranchCreateFailedData::Failed { branch, error, .. } => (branch, error),
                    BranchCreateFailedData::Cancelled { branch, reason, .. } => (branch, reason),
                };
                let history = self.task_history.entry(branch.clone()).or_default();
                let newly_failed = !history.done;
                history.done = true;
                history.error = None;
                history.live.clear();
                history.window_complete = false;
                self.task_history_queue.retain(|id| id != branch);
                if let Some(request) = &mut self.task_history_request
                    && request.branch_id == *branch
                {
                    request.current = false;
                }
                if newly_failed {
                    self.send(Update::Notice(format!("Branch {branch}: {error}")));
                }
            }
            _ => {}
        }
    }

    pub(super) fn task_event(&mut self, branch_id: &str, event: Event) {
        self.discover_branch(branch_id);
        self.discover_task_branches(&event);
        let shown = branch_id == self.focus || self.side.as_deref() == Some(branch_id);
        let applied = self.cursors.get(branch_id).copied().unwrap_or(0);
        let history = self
            .task_history
            .get_mut(branch_id)
            .expect("discovered branch");
        let mut unread_changed = false;
        if branch_id != "main"
            && event.sequence > applied.max(history.latest_reply)
            && matches!(&event.content, EventContent::Text(data) if !data.delta.trim().is_empty())
        {
            unread_changed = !shown && history.latest_reply == history.seen_reply;
            history.latest_reply = event.sequence;
            if shown {
                history.seen_reply = event.sequence;
            }
        }
        if history.done {
            history.goal_stale |= crate::goal::changed(&event.content);
            self.apply(branch_id, event);
        } else {
            history.live.push(event);
            if unread_changed {
                self.changed();
            }
        }
    }

    pub(super) fn retry_task_history(&mut self) {
        for (id, history) in &mut self.task_history {
            if history.error.take().is_some() {
                if history.earlier == EarlierRead::Failed {
                    history.earlier = EarlierRead::Reading;
                }
                self.task_history_queue.push_back(id.clone());
            }
        }
        self.changed();
    }

    pub(super) fn earlier(&mut self) {
        if self.earlier_member() {
            return;
        }
        let Some(history) = self.task_history.get_mut(&self.focus) else {
            return;
        };
        if !history.done || history.earlier == EarlierRead::Reading {
            return;
        }
        if history.window.before.is_none() {
            self.send(Update::Notice("Beginning of conversation".into()));
            return;
        }
        history.earlier = EarlierRead::Reading;
        history.window_complete = false;
        history.error = None;
        self.task_history_queue.push_back(self.focus.clone());
    }

    /// Whether the view `earlier()` would act on has older history to read.
    /// It reads the same fields `earlier()` tests: `earlier_member` for a
    /// selected member, otherwise `task_history[focus]` in `earlier`.
    pub(super) fn earlier_view(&self) -> Earlier {
        if self.members.selected.is_some() {
            return self.earlier_observation();
        }
        self.task_history
            .get(&self.focus)
            .map_or(Earlier::AtStart, |history| {
                if history.earlier == EarlierRead::Reading {
                    Earlier::Reading
                } else if history.window.before.is_some() {
                    Earlier::Unread
                } else {
                    Earlier::AtStart
                }
            })
    }

    pub(super) async fn poll_task_history(&mut self) -> crate::Result<()> {
        if self.task_history_request.is_some() || self.session_id.is_empty() {
            return Ok(());
        }
        if let Some(branch) = self
            .task_history
            .iter()
            .find_map(|(id, state)| state.goal_stale.then(|| id.clone()))
        {
            self.task_history.get_mut(&branch).unwrap().goal_stale = false;
            if self.methods.contains("session.goal.get") {
                match self
                    .client
                    .send(
                        "session.goal.get",
                        json!({"sessionId":self.session_id,"branchId":branch}),
                    )
                    .await
                {
                    Ok(id) => {
                        self.task_history_request = Some(HistoryRequest {
                            id,
                            session_id: self.session_id.clone(),
                            branch_id: branch,
                            current: true,
                            stage: ReadStage::Goal,
                        })
                    }
                    Err(error) => self.send(Update::Notice(format!("Goal: {error}"))),
                }
                return Ok(());
            }
        }
        let Some(branch_id) = self.task_history_queue.pop_front() else {
            if self.methods.contains("session.tool.get") {
                let mut shown = vec![self.focus.clone()];
                if let Some(side) = &self.side
                    && side != &self.focus
                {
                    shown.push(side.clone());
                }
                for branch in shown {
                    if !self
                        .task_history
                        .get(&branch)
                        .is_some_and(|history| history.done)
                    {
                        continue;
                    }
                    let missing = transcript_mut(&mut self.projection, &mut self.lines, &branch)
                        .and_then(|projection| projection.missing_tool());
                    if let Some((request_id, call_id)) = missing {
                        if let Some(projection) =
                            transcript_mut(&mut self.projection, &mut self.lines, &branch)
                        {
                            projection.tool_lookup(&request_id, &call_id, None);
                        }
                        match self.client.send("session.tool.get",json!({"sessionId":self.session_id,"branchId":branch,"requestId":request_id,"callId":call_id})).await {
                            Ok(id)=>self.task_history_request=Some(HistoryRequest { id,session_id:self.session_id.clone(),branch_id:branch,current:true,stage:ReadStage::ToolOrigin { request_id,call_id } }),
                            Err(error)=>self.send(Update::Notice(format!("Tool origin: {error}"))),
                        }
                        break;
                    }
                }
            }
            return Ok(());
        };
        if !self.client.supports(&["session.history.latestTurn"]) {
            return Err(
                "This daemon cannot read a bounded transcript window; update the daemon".into(),
            );
        }
        let (stage, method, params) = if branch_id != "main"
            && !self.lines.contains_key(&branch_id)
            && !self.task_history[&branch_id].window_complete
        {
            (
                ReadStage::Cut,
                "session.turn.get",
                json!({"sessionId":self.session_id,"branchId":branch_id}),
            )
        } else if self.task_history[&branch_id].window_complete {
            for required in ["session.input.pending", "session.turn.get"] {
                if !self.methods.contains(required) {
                    self.fail_inventory(
                        &branch_id,
                        format!("Current-state read unavailable: {required}"),
                    );
                    return Ok(());
                }
            }
            let history = self.task_history.get_mut(&branch_id).unwrap();
            while history.inventory_index < inventory::METHODS.len()
                && !self
                    .methods
                    .contains(inventory::METHODS[history.inventory_index])
            {
                history.inventory_index += 1;
            }
            if history.inventory_index == inventory::METHODS.len() {
                self.finish_window(&branch_id);
                return Ok(());
            }
            let method = inventory::METHODS[history.inventory_index];
            let mut params = json!({"sessionId":self.session_id,"branchId":branch_id});
            if method != "session.goal.get" {
                params["through"] = json!(history.window.through.unwrap());
            }
            if let Some(after) = &history.inventory_cursor {
                params["after"] = after.clone();
            }
            (ReadStage::Inventory(method), method, params)
        } else {
            (
                ReadStage::History,
                if self
                    .client
                    .supports(&["session.history.display", "session.history.detail"])
                {
                    "session.history.display"
                } else {
                    "session.history"
                },
                self.task_history[&branch_id]
                    .window
                    .params(&self.session_id, &branch_id),
            )
        };
        match self.client.send(method, params).await {
            Ok(id) => {
                self.task_history_request = Some(HistoryRequest {
                    id,
                    session_id: self.session_id.clone(),
                    branch_id,
                    current: true,
                    stage,
                })
            }
            Err(error) => {
                let history = self.task_history.get_mut(&branch_id).unwrap();
                history.error = Some(error.to_string());
                self.changed();
            }
        }
        Ok(())
    }

    pub(super) async fn task_history_reply(
        &mut self,
        result: Result<serde_json::Value, client::RpcError>,
    ) -> crate::Result<()> {
        let request = self
            .task_history_request
            .take()
            .expect("matched history request");
        if !request.current || request.session_id != self.session_id {
            return Ok(());
        }
        if let ReadStage::ToolOrigin {
            request_id,
            call_id,
        } = &request.stage
        {
            let read = result.map_err(|error| error.to_string()).and_then(|value| {
                serde_json::from_value::<dim_agent_types::ToolRead>(value)
                    .map_err(|error| error.to_string())
            });
            match read {
                Ok(read)
                    if read.branch_id == request.branch_id
                        && read.call.as_ref().is_none_or(|call| {
                            call.request_id == *request_id && call.call.id == *call_id
                        }) =>
                {
                    if let Some(projection) =
                        transcript_mut(&mut self.projection, &mut self.lines, &request.branch_id)
                    {
                        projection.tool_lookup(request_id, call_id, read.call);
                    }
                }
                Ok(_) => self.send(Update::Notice("Tool origin identity changed".into())),
                Err(error) => self.send(Update::Notice(format!("Tool origin: {error}"))),
            }
            self.changed();
            return Ok(());
        }
        if matches!(request.stage, ReadStage::Goal) {
            let result = result.map_err(|error| error.to_string()).and_then(|value| {
                self.apply_inventory(&request.branch_id, 0, "session.goal.get", value)
                    .map_err(|error| error.to_string())
            });
            if let Err(error) = result {
                self.send(Update::Notice(format!("Goal: {error}")));
            }
            self.changed();
            return Ok(());
        }
        if !matches!(request.stage, ReadStage::History) {
            let result = result.map_err(|error| error.to_string()).and_then(|value| {
                match request.stage {
                    ReadStage::Cut => {
                        let turn: crate::wire::TurnRead =
                            serde_json::from_value(value).map_err(|error| error.to_string())?;
                        if turn.branch_id != request.branch_id {
                            return Err("Inventory branch changed".into());
                        }
                        let history = self.task_history.get_mut(&request.branch_id).unwrap();
                        history.window.through = Some(turn.through);
                        history.window_complete = true;
                    }
                    ReadStage::Inventory(method) => {
                        let through = self.task_history[&request.branch_id]
                            .window
                            .through
                            .unwrap();
                        let next = self
                            .apply_inventory(&request.branch_id, through, method, value)
                            .map_err(|error| error.to_string())?;
                        let history = self.task_history.get_mut(&request.branch_id).unwrap();
                        if next.is_some() && next == history.inventory_cursor {
                            return Err("Inventory cursor did not advance".into());
                        }
                        if next.is_none() {
                            history.inventory_index += 1;
                        }
                        history.inventory_cursor = next;
                    }
                    ReadStage::History | ReadStage::Goal | ReadStage::ToolOrigin { .. } => {
                        unreachable!()
                    }
                }
                Ok(())
            });
            match result {
                Ok(()) => self.task_history_queue.push_back(request.branch_id),
                Err(error) => self.fail_inventory(&request.branch_id, error),
            }
            self.changed();
            return Ok(());
        }
        let code = result
            .as_ref()
            .err()
            .and_then(|error| error.data.as_ref())
            .and_then(|data| data["appError"].as_str())
            .map(str::to_owned);
        let history = self
            .task_history
            .get_mut(&request.branch_id)
            .expect("current history");
        let result = result.map_err(|error| error.to_string()).and_then(|value| {
            let page = if self
                .client
                .supports(&["session.history.display", "session.history.detail"])
            {
                serde_json::from_value::<dim_client::wire::DisplayPage>(value).map(Into::into)
            } else {
                serde_json::from_value(value)
            }
            .map_err(|error| format!("Invalid history reply: {error}"))?;
            history.window.push(page).map_err(|error| error.to_string())
        });
        match result {
            Err(error) => {
                history.error = Some(error.clone());
                if history.earlier == EarlierRead::Reading {
                    history.earlier = EarlierRead::Failed;
                }
                let fatal = request.branch_id == "main" && !history.done;
                let report = crate::report::Captured::observed(
                    crate::report::Operation::SessionRead,
                    code.as_deref(),
                    format!("Could not read conversation state: {error}"),
                );
                if !self.reportable(report, true) && fatal {
                    return Err(format!("Main branch history: {error}").into());
                }
            }
            Ok(false) => self.task_history_queue.push_back(request.branch_id),
            Ok(true) => {
                let events = history.window.take();
                let missing_prefix = history.window.before.is_some();
                history.window_complete = true;
                if history.earlier == EarlierRead::Reading {
                    history.earlier = EarlierRead::None;
                    let mut older = Projection::for_branch(&request.branch_id);
                    for event in events {
                        older.replay(&request.branch_id, &event);
                    }
                    if let Some(projection) =
                        transcript_mut(&mut self.projection, &mut self.lines, &request.branch_id)
                    {
                        projection.prepend(older);
                    }
                } else {
                    for event in events {
                        self.replay(&request.branch_id, event);
                    }
                    self.task_history_queue.push_back(request.branch_id.clone());
                }
                if missing_prefix {
                    self.send(Update::Notice(
                        "Earlier content not loaded; /earlier to read it".into(),
                    ));
                }
            }
        }
        self.changed();
        Ok(())
    }

    fn fail_inventory(&mut self, branch: &str, error: String) {
        let history = self.task_history.get_mut(branch).unwrap();
        history.error = Some(error);
        history.inventory_index = 0;
        history.inventory_cursor = None;
        self.projection.clear_inventory(branch);
        if let Some(line) = self.lines.get_mut(branch) {
            line.projection.clear_inventory(branch);
        }
        self.changed();
    }

    fn finish_window(&mut self, branch: &str) {
        let through = self.task_history[branch]
            .window
            .through
            .expect("loaded window");
        self.cursors.insert(branch.to_owned(), through);
        if let Some(line) = self.lines.get_mut(branch) {
            line.cursor = through;
        }
        let history = self.task_history.get_mut(branch).unwrap();
        history.done = true;
        history.error = None;
        let mut live = std::mem::take(&mut history.live);
        let running = history.running;
        live.sort_by_key(|event| event.sequence);
        if let Some(projection) = transcript_mut(&mut self.projection, &mut self.lines, branch)
            && let Some(running) = running
        {
            projection.set_running(running);
        }
        for event in live {
            if event.sequence > through {
                self.task_event(branch, event);
            }
        }
        if let Some(projection) = transcript_mut(&mut self.projection, &mut self.lines, branch) {
            projection.trim(300);
        }
        self.changed();
    }

    pub(super) fn stop_tasks(&mut self, session_id: String, projection_id: u64) {
        if self.task_stops.is_some() {
            self.send(Update::Notice(
                "Stop-all requests are already in progress".into(),
            ));
            return;
        }
        if self.session_id.is_empty()
            || session_id != self.session_id
            || projection_id != self.projection_id
        {
            self.send(Update::Notice(
                "Task attachment changed; reopen the panel before stopping work".into(),
            ));
            return;
        }
        if self.task_history.values().any(|history| !history.done) {
            self.send(Update::Notice(
                "Branch history is incomplete; stop-all unavailable".into(),
            ));
            return;
        }
        if self.workflow_incomplete() {
            self.send(Update::Notice(
                "Workflow inventory is incomplete; stop-all unavailable".into(),
            ));
            return;
        }
        let stop_workflows = self.client.supports(&["workflow.stop"]);
        let mut members = HashSet::new();
        let mut targets = VecDeque::new();
        let mut workflows = Vec::new();
        let mut unavailable = 0;
        for task in self
            .projection
            .tasks()
            .iter()
            .filter(|task| task.finished.is_none())
        {
            match &task.kind {
                TaskKind::Command(_) => targets.push_back(task.kind.clone()),
                TaskKind::Member(MemberTarget::Session(id)) if members.insert(id.clone()) => {
                    targets.push_back(task.kind.clone())
                }
                TaskKind::Member(MemberTarget::Session(_)) => {}
                TaskKind::Member(MemberTarget::Starting | MemberTarget::Provider) => {
                    unavailable += 1
                }
                TaskKind::Workflow(view)
                    if stop_workflows
                        && view.run.state == crate::wire::WorkflowState::Running
                        && !view.stop_pending =>
                {
                    workflows.push(task.kind.clone())
                }
                _ => {}
            }
        }
        targets.extend(workflows);
        self.task_stops = Some(StopBatch {
            session_id,
            projection_id,
            targets,
            request: None,
            attempted: 0,
            errors: 0,
            unavailable,
        });
    }

    pub(super) async fn poll_task_stops(&mut self) {
        let Some(batch) = &mut self.task_stops else {
            return;
        };
        if batch.request.is_some() {
            return;
        }
        let Some(target) = batch.targets.pop_front() else {
            let batch = self.task_stops.take().expect("finished stop batch");
            self.report(format!(
                "Stop-all requests for {}: attempted {}, errors {}, unavailable {}.\nStarting members and new work may continue. Profile-supplied member background commands keep running. Teammate Steer messages may wake members. A non-cancelled end of such a command may also wake a member. Unsuppressed reports may wake or steer the leader. The leader may start new work during or after stop-all. Rows wait for real terminal facts.",
                batch.session_id, batch.attempted, batch.errors, batch.unavailable,
            ), batch.errors != 0);
            return;
        };
        let mut workflow = None;
        let (label, method, params) = match target {
            TaskKind::Workflow(view) => {
                let run_id = view.run.run_id;
                workflow = Some(run_id.clone());
                (
                    format!("workflow {run_id}"),
                    "workflow.stop",
                    json!({"run_id": run_id}),
                )
            }
            TaskKind::Command(target) => (
                format!("command {}/{}", target.branch_id, target.task_id),
                "session.exec.stop",
                json!({
                    "sessionId": batch.session_id, "branchId": target.branch_id, "taskId": target.task_id,
                }),
            ),
            TaskKind::Member(MemberTarget::Session(session_id)) => (
                format!("member {session_id}"),
                "session.stop",
                json!({
                    "sessionId": session_id, "branchId": "main", "reason": "stopped by the user",
                }),
            ),
            TaskKind::Member(MemberTarget::Starting | MemberTarget::Provider) => {
                unreachable!("admission excludes targets without a member Session")
            }
        };
        batch.attempted += 1;
        let current = batch.projection_id == self.projection_id;
        match self.client.send(method, params).await {
            Ok(id) => {
                if current && let Some(run_id) = &workflow {
                    self.projection.workflow_stop_pending(run_id, true);
                    self.changed();
                }
                self.task_stops.as_mut().expect("stop batch").request = Some((id, label, workflow));
            }
            Err(error) => {
                batch.errors += 1;
                self.send(Update::Notice(format!("Stop-all {label}: {error}")));
                self.changed();
            }
        }
    }

    pub(super) fn task_stop_reply(&mut self, result: Result<serde_json::Value, client::RpcError>) {
        let batch = self.task_stops.as_mut().expect("matched stop reply");
        let (_, target, workflow) = batch.request.take().expect("matched stop target");
        let result = match workflow {
            Some(run_id) if batch.projection_id == self.projection_id => {
                self.workflow_stopped(&run_id, result)
            }
            _ => result.map(drop).map_err(|error| error.to_string()),
        };
        if let Err(error) = result {
            self.task_stops.as_mut().expect("stop batch").errors += 1;
            self.send(Update::Notice(format!("Stop-all {target}: {error}")));
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::Message;

    /// One `step/text` event, as the daemon sends it on the wire and in a page.
    fn text(sequence: u64, request: &str, delta: &str) -> serde_json::Value {
        json!({
            "id": format!("text-{sequence}"), "sequence": sequence, "create_at": 2000,
            "type": "step", "name": "text",
            "data": {"turn_id": "t2", "request_id": request, "delta": delta}
        })
    }

    /// The same event, decoded as the live stream decodes it.
    fn event(sequence: u64, request: &str, delta: &str) -> Event {
        serde_json::from_value(text(sequence, request, delta)).expect("a text event")
    }

    /// Answers the outstanding history read with the daemon's failure.
    async fn fail(worker: &mut Worker, what: &str) {
        let id = worker
            .task_history_request
            .as_ref()
            .unwrap_or_else(|| panic!("{what} was sent"))
            .id;
        worker
            .message(Message::Response {
                id,
                result: Err(client::RpcError {
                    code: -32603,
                    message: "the daemon dropped the read".into(),
                    data: None,
                }),
            })
            .await
            .unwrap();
    }

    /// Answers the outstanding history read with a page.
    async fn answer(worker: &mut Worker, page: serde_json::Value) {
        let id = worker
            .task_history_request
            .as_ref()
            .expect("the read in flight")
            .id;
        worker
            .message(Message::Response {
                id,
                result: Ok(page),
            })
            .await
            .unwrap();
    }

    /// A branch whose earlier read failed reports `Unread` and reads again on
    /// the next press. A failure that kept the read recorded as in flight makes
    /// `earlier_view` report `Reading` with nothing sent, and `earlier` returns
    /// at its own state: PageUp, the load-more row and `/earlier` never read
    /// that line again.
    #[tokio::test]
    async fn a_failed_earlier_read_returns_the_branch_to_unread_and_reads_again() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("main");
        let history = worker.task_history.get_mut("main").unwrap();
        history.done = true;
        history.window.before = Some(7);
        history.window.through = Some(9);
        assert_eq!(
            worker.earlier_view(),
            Earlier::Unread,
            "the loaded window has older history"
        );

        worker.earlier();
        assert_eq!(
            worker.earlier_view(),
            Earlier::Reading,
            "the earlier read is in flight"
        );
        worker.poll_task_history().await.unwrap();
        fail(&mut worker, "the earlier read").await;

        assert_eq!(
            worker.earlier_view(),
            Earlier::Unread,
            "a failed earlier read leaves the older history to read"
        );
        worker.earlier();
        assert_eq!(
            worker.task_history["main"].earlier,
            EarlierRead::Reading,
            "the next press records another earlier read"
        );
        assert!(
            worker.task_history_queue.contains(&"main".to_owned()),
            "the next press queues a read"
        );
    }

    /// A `/tasks` retry of a failed earlier read reads the same older request
    /// the window still holds, and the rows it brings go ahead of the ones
    /// already loaded. A retry that records no earlier read folds the page as a
    /// first window instead: every event in it is at or below the line's
    /// cutoff, so none of them reach the transcript.
    #[tokio::test]
    async fn a_retried_earlier_read_prepends_the_rows_it_reads() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.discover_branch("b-7c");
        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        {
            let history = worker.task_history.get_mut("b-7c").unwrap();
            history.done = true;
            history.window.before = Some(200);
            history.window.through = Some(300);
        }
        worker.apply("b-7c", event(300, "r-loaded", "the answer loaded first"));

        worker.earlier();
        worker.poll_task_history().await.unwrap();
        fail(&mut worker, "the earlier read").await;

        worker
            .command(crate::model::session::Command::TaskHistory {
                projection_id: worker.projection_id,
            })
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        assert!(
            worker.task_history_request.is_some(),
            "the retried read is in flight"
        );
        assert_eq!(
            worker.task_history["b-7c"].window.before,
            Some(200),
            "the retry is the older request the window still holds"
        );
        worker.task_history_queue.clear();
        let older: Vec<_> = (2..=101)
            .map(|sequence| text(sequence, &format!("r-older-{sequence}"), "older "))
            .collect();
        answer(
            &mut worker,
            json!({"events": older, "through": 300, "nextBefore": 2, "nextAfter": 101}),
        )
        .await;

        let rows = worker.lines["b-7c"].projection.rows();
        assert_eq!(rows.len(), 101, "every row of the page joined the line");
        assert_eq!(
            rows[0].text, "older ",
            "the page's first row is ahead of everything loaded"
        );
        assert_eq!(
            rows[100].text, "the answer loaded first",
            "the loaded rows follow the older page"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7c".to_owned()),
            "the page was an older page, not a first window"
        );
        assert_eq!(
            worker.earlier_view(),
            Earlier::Unread,
            "the window still starts after the event before the page"
        );
        assert_eq!(
            worker.task_history["b-7c"].window.before,
            Some(2),
            "the window holds the older request the page ended at"
        );
    }

    /// Opening a branch's line after its earlier read failed starts the line
    /// over: the window that read was asked for is gone with the reset. A
    /// failure left in place outlives the window it belonged to, and a retry
    /// from the tasks panel then folds the line's first window as an older
    /// page, which prepends rows below the cutoff and never reads it on.
    #[tokio::test]
    async fn a_line_opened_after_a_failed_earlier_read_records_no_read() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.discover_branch("b-7c");
        {
            let history = worker.task_history.get_mut("b-7c").unwrap();
            history.done = true;
            history.window.before = Some(7);
            history.window.through = Some(9);
            history.earlier = EarlierRead::Failed;
        }

        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();

        assert_eq!(
            worker.task_history["b-7c"].earlier,
            EarlierRead::None,
            "the line starts over, so nothing earlier is recorded against it"
        );
        assert_eq!(
            worker.earlier_view(),
            Earlier::AtStart,
            "the line's window starts from nothing, so it reports its start"
        );
    }

    /// A retried earlier read is an earlier read again: the line reports
    /// `Reading` while it is in flight, and a press during it queues nothing.
    #[tokio::test]
    async fn a_tasks_retry_of_an_earlier_read_reports_reading() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("main");
        {
            let history = worker.task_history.get_mut("main").unwrap();
            history.done = true;
            history.window.before = Some(7);
            history.window.through = Some(9);
        }
        worker.earlier();
        worker.poll_task_history().await.unwrap();
        fail(&mut worker, "the earlier read").await;

        worker
            .command(crate::model::session::Command::TaskHistory {
                projection_id: worker.projection_id,
            })
            .await
            .unwrap();
        assert_eq!(
            worker.earlier_view(),
            Earlier::Reading,
            "the retried read is the earlier read the window still holds"
        );
        worker.task_history_queue.clear();
        worker.earlier();
        assert!(
            worker.task_history_queue.is_empty(),
            "a press during the retried read queues nothing"
        );
    }
}

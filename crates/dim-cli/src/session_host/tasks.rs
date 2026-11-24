//! Finite history pages and addressed stops owned by the Session worker.

use std::collections::{HashSet, VecDeque};

use dim_agent_types::{BranchCreateFailedData, ROOT_BRANCH_ID};

use super::*;
use crate::model::view::{MemberTarget, TaskKind};

#[derive(Default)]
pub(super) struct BranchHistory {
    pub(super) window: dim_client::window::Window,
    pub done: bool,
    pub ended: bool,
    pub error: Option<String>,
    live: Vec<Event>,
    pub(super) window_complete: bool,
    /// The first window's failed read; the line shows it instead of a
    /// transcript until the line is opened again. A later read (an inventory
    /// refresh or an `/earlier` page) records `error` and leaves the loaded
    /// window in place.
    pub(super) window_error: Option<String>,
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

impl HistoryRequest {
    pub(super) fn is_tool_origin(&self, branch: &str, request: &str, call: &str) -> bool {
        self.branch_id == branch
            && matches!(
                &self.stage,
                ReadStage::ToolOrigin { request_id, call_id }
                    if request_id == request && call_id == call
            )
    }
}

impl Worker {
    /// The replacement releases this attachment after creating or finding its Session.
    pub(super) fn invalidate_projection(&mut self) -> String {
        self.opening_reads.clear();
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
        self.tool_origins.clear();
        self.default_branch = ROOT_BRANCH_ID.into();
        self.line_configurations.clear();
        self.unreadable_line = None;
        self.focus = DEFAULT_BRANCH_ID.into();
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
        let creation_failed = self.projection.creation_failed(branch_id);
        let history = self
            .task_history
            .get_mut(branch_id)
            .expect("discovered line");
        history.seen_reply = history.latest_reply;
        if !creation_failed
            && ((!self.lines.contains_key(branch_id)
                && (history.window.through.is_some() || history.done || history.error.is_some()))
                || history.window_error.is_some())
        {
            history.window = Default::default();
            history.done = false;
            history.error = None;
            history.window_complete = false;
            history.window_error = None;
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
            EventContent::BranchEnded(data) => {
                self.task_history
                    .entry(data.branch.clone())
                    .and_modify(|history| history.ended = true)
                    .or_insert_with(|| BranchHistory {
                        ended: true,
                        done: true,
                        ..Default::default()
                    });
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
                history.window_error = None;
                self.task_history_queue.retain(|id| id != branch);
                if let Some(request) = &mut self.task_history_request
                    && request.branch_id == *branch
                {
                    request.current = false;
                }
                if newly_failed {
                    self.send(Update::Notice(format!("Branch {branch}: {error}")));
                }
                // Terminal for the directory as soon as the fact arrives; the
                // transcript still waits for the window that recorded it.
                let first_terminal = !self.projection.creation_failed(branch);
                self.projection.apply(branch, event);
                if self.focus == *branch {
                    let parent = self
                        .projection
                        .line_views()
                        .iter()
                        .find(|line| line.id == *branch)
                        .and_then(|line| line.parent.clone())
                        .filter(|parent| {
                            parent.as_str() == DEFAULT_BRANCH_ID
                                || self.task_history.contains_key(parent)
                        })
                        .unwrap_or_else(|| DEFAULT_BRANCH_ID.into());
                    if parent != DEFAULT_BRANCH_ID {
                        self.line_projection(&parent);
                    }
                    self.focus = parent;
                    self.changed();
                } else if first_terminal {
                    // `/lines` draws the directory from published snapshots.
                    self.changed();
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
        history.ended |= matches!(event.content, EventContent::BranchEnd(_));
        let mut unread_changed = false;
        if branch_id != self.default_branch
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
                history.window_error = None;
                if history.earlier == EarlierRead::Failed {
                    history.earlier = EarlierRead::Reading;
                }
                self.task_history_queue.push_back(id.clone());
            }
        }
        self.changed();
    }

    pub(super) fn earlier(&mut self, line: &str) {
        if self.earlier_member() {
            return;
        }
        let Some(history) = self.task_history.get_mut(line) else {
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
        history.error = None;
        self.task_history_queue.push_back(line.to_owned());
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
        if let Some((branch_id, request_id, call_id)) = self.tool_origins.pop_front() {
            if !transcript_mut(&mut self.projection, &mut self.lines, &branch_id).is_some_and(
                |projection| {
                    projection.rows().iter().any(|row| {
                        row.batch.as_deref() == Some(&request_id)
                            && row.call_id.as_deref() == Some(&call_id)
                            && !row.tool_lookup_done
                    })
                },
            ) {
                return Ok(());
            }
            let params = json!({
                "sessionId": self.session_id,
                "branchId": branch_id,
                "requestId": request_id,
                "callId": call_id,
            });
            match self.client.send("session.tool.get", params).await {
                Ok(id) => {
                    self.task_history_request = Some(HistoryRequest {
                        id,
                        session_id: self.session_id.clone(),
                        branch_id,
                        current: true,
                        stage: ReadStage::ToolOrigin {
                            request_id,
                            call_id,
                        },
                    })
                }
                Err(error) => {
                    if let Some(projection) =
                        transcript_mut(&mut self.projection, &mut self.lines, &branch_id)
                    {
                        projection.tool_lookup(&request_id, &call_id, None);
                    }
                    self.send(Update::Notice(format!("Tool origin: {error}")));
                    self.changed();
                }
            }
            return Ok(());
        }
        let Some(branch_id) = self.task_history_queue.pop_front() else {
            return Ok(());
        };
        if !self.client.supports(&["session.history.latestTurn"]) {
            return Err(
                "This daemon cannot read a bounded transcript window; update the daemon".into(),
            );
        }
        let earlier_reading = self.task_history[&branch_id].earlier == EarlierRead::Reading;
        let window_read = earlier_reading || !self.task_history[&branch_id].window_complete;
        let (stage, method, params) = if branch_id != self.default_branch
            && !self.lines.contains_key(&branch_id)
            && !self.task_history[&branch_id].window_complete
        {
            (
                ReadStage::Cut,
                "session.turn.get",
                json!({"sessionId":self.session_id,"branchId":branch_id}),
            )
        } else if !window_read {
            for required in ["session.input.pending", "session.turn.get"] {
                if !self.task_history[&branch_id].ended && !self.methods.contains(required) {
                    self.fail_inventory(
                        &branch_id,
                        format!("Current-state read unavailable: {required}"),
                    );
                    return Ok(());
                }
            }
            let history = self.task_history.get_mut(&branch_id).unwrap();
            while history.inventory_index < inventory::METHODS.len()
                && (!self
                    .methods
                    .contains(inventory::METHODS[history.inventory_index])
                    || (history.ended
                        && !matches!(
                            inventory::METHODS[history.inventory_index],
                            "session.exec.finished" | "session.collaboration.finished"
                        )))
            {
                history.inventory_index += 1;
            }
            if history.inventory_index == inventory::METHODS.len() {
                self.finish_window(&branch_id);
                return Ok(());
            }
            let method = inventory::METHODS[history.inventory_index];
            let mut params = json!({"sessionId":self.session_id,"branchId":branch_id});
            if method == "session.input.pending" {
                if !self
                    .client
                    .supports(&["session.input.pending.blockPresence"])
                {
                    return Err(
                        "This daemon cannot distinguish user queue inputs; update the daemon"
                            .into(),
                    );
                }
                params["includeBlockPresence"] = json!(true);
            }
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
                self.task_history[&branch_id].window.params_with_display(
                    &self.session_id,
                    &branch_id,
                    self.client.offers("session.history.display.tail"),
                    self.client.offers("session.history.display.transcript"),
                ),
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
                match stage {
                    ReadStage::History => self.fail_history(&branch_id, error.to_string()),
                    _ => {
                        self.task_history.get_mut(&branch_id).unwrap().error =
                            Some(error.to_string());
                    }
                }
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
                Ok(_) => {
                    if let Some(projection) =
                        transcript_mut(&mut self.projection, &mut self.lines, &request.branch_id)
                    {
                        projection.tool_lookup(request_id, call_id, None);
                    }
                    self.send(Update::Notice("Tool origin identity changed".into()));
                }
                Err(error) => {
                    if let Some(projection) =
                        transcript_mut(&mut self.projection, &mut self.lines, &request.branch_id)
                    {
                        projection.tool_lookup(request_id, call_id, None);
                    }
                    self.send(Update::Notice(format!("Tool origin: {error}")));
                }
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
                        self.projection.missing_history(&request.branch_id);
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
        let fatal =
            request.branch_id == self.default_branch && !self.task_history[&request.branch_id].done;
        let display = self
            .client
            .supports(&["session.history.display", "session.history.detail"]);
        let transcript = self.client.offers("session.history.display.transcript");
        let mut at_start = false;
        let result = result.map_err(|error| error.to_string()).and_then(|value| {
            let page = if display {
                let page: dim_client::wire::DisplayPage = serde_json::from_value(value)
                    .map_err(|error| format!("Invalid history reply: {error}"))?;
                at_start = transcript && page.done();
                page.into()
            } else {
                serde_json::from_value(value)
                    .map_err(|error| format!("Invalid history reply: {error}"))?
            };
            self.task_history
                .get_mut(&request.branch_id)
                .expect("current history")
                .window
                .push(page)
                .map_err(|error| error.to_string())
        });
        let window_ready = matches!(result, Ok(true));
        let mut missing_prefix = false;
        match result {
            Err(error) => {
                if fatal {
                    return Err(format!("Main branch history: {error}").into());
                }
                self.fail_history(&request.branch_id, error.clone());
                let report = crate::report::Captured::observed(
                    crate::report::Operation::SessionRead,
                    code.as_deref(),
                    format!("Could not read conversation state: {error}"),
                );
                self.reportable(report, true);
            }
            Ok(false) => self.task_history_queue.push_back(request.branch_id.clone()),
            Ok(true) => {
                let history = self
                    .task_history
                    .get_mut(&request.branch_id)
                    .expect("current history");
                history.window_error = None;
                let events = history.window.take();
                missing_prefix =
                    history.window.dropped_pages() || history.window.before.is_some() && !at_start;
                history.window_complete = true;
                if history.earlier == EarlierRead::Reading {
                    history.earlier = EarlierRead::None;
                    let mut older = Projection::for_branch(&request.branch_id);
                    for event in events {
                        self.projection.line_event(&request.branch_id, &event);
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
                    self.projection.missing_history(&request.branch_id);
                } else {
                    self.projection.whole_history(&request.branch_id);
                }
            }
        }
        self.changed();
        // A display transcript omits `context/configured`, and a bounded window
        // can start after it: the shown line's configuration is read through
        // `session.model.get` once its window is ready, unless a later fact
        // already set it (design §14). A server without the read names no
        // configuration, and the reply's is the root's, so the line carries
        // no model until a fact or a move supplies one.
        if window_ready
            && self.model.is_none()
            && request.branch_id == self.default_branch
            && self.default_branch != ROOT_BRANCH_ID
        {
            let through = self
                .task_history
                .get(&request.branch_id)
                .and_then(|history| history.window.through);
            if let Some((cutoff, configuration)) =
                self.line_configuration(&request.branch_id, through).await
                && request.branch_id == self.default_branch
                && cutoff > self.model_through
            {
                self.model_through = cutoff;
                if let Some(configuration) = configuration {
                    self.model = Some(configuration.clone());
                    self.line_configurations.insert(
                        request.branch_id.clone(),
                        LineConfiguration {
                            sequence: cutoff,
                            configuration: configuration.clone(),
                        },
                    );
                    self.send(Update::Ready(configuration));
                }
            }
            if self.model.is_none() {
                self.send(Update::NoModel);
            }
        }
        if window_ready {
            self.publish();
        }
        if missing_prefix {
            self.send(Update::Notice(
                "Earlier content not loaded; /earlier to read it".into(),
            ));
        }
        Ok(())
    }

    /// Records a failed history read. A first window that never loaded is a
    /// window failure; a failed older page leaves the loaded window in place
    /// and returns `/earlier` to its unread state.
    fn fail_history(&mut self, branch: &str, error: String) {
        let history = self.task_history.get_mut(branch).expect("a line's history");
        history.error = Some(error.clone());
        if history.earlier == EarlierRead::Reading {
            history.earlier = EarlierRead::Failed;
        } else if !history.done && !history.window_complete {
            history.window_error = Some(error);
        }
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
            if event.sequence > through || matches!(event.content, EventContent::Configuration(_)) {
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
                    "sessionId": session_id, "reason": "stopped by the user",
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
    use crate::model::view::ViewSnapshot;

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

    /// One user message, as the daemon sends it on the wire and in a page.
    fn said(sequence: u64, message: &str) -> serde_json::Value {
        json!({
            "id": format!("in-{sequence}"), "sequence": sequence, "create_at": 2000,
            "type": "input", "name": "message",
            "data": {"user": {"content": [{"type": "text", "text": message}]}}
        })
    }

    /// The withdrawal of `input`, as the daemon sends it on the wire and in a page.
    fn input_deleted(sequence: u64, input: &str) -> serde_json::Value {
        json!({
            "id": format!("deleted-{sequence}"), "sequence": sequence, "create_at": 2000,
            "type": "input", "name": "deleted", "data": {"input_id": input}
        })
    }

    /// The creation of `branch` in `parent`'s history, cut after `at`.
    fn line_created(sequence: u64, branch: &str, at: u64) -> serde_json::Value {
        json!({
            "id": format!("create-{branch}"), "sequence": sequence, "create_at": 2000,
            "type": "branch", "name": "create",
            "data": {
                "branch": branch, "at": {"kind": "sequence", "sequence": at},
                "source": {"kind": "tool", "turn_id": "t1", "request_id": "r1", "call_id": "c1"},
                "prompt": branch,
            }
        })
    }

    /// The same `input` or `branch` event, decoded as the live stream decodes it.
    fn tell(event: serde_json::Value) -> Event {
        serde_json::from_value(event).expect("a Session event")
    }

    /// The line `id` of the Session's line list.
    fn listed(worker: &Worker, id: &str) -> crate::model::view::LineView {
        worker
            .projection
            .line_views()
            .iter()
            .find(|line| line.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("line {id} is listed"))
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

    /// A `branch/create-failed` event for `branch`, as the daemon sends it.
    fn create_failed(sequence: u64, branch: &str) -> Event {
        serde_json::from_value(create_failed_page(sequence, branch)).expect("a create-failed event")
    }

    /// The wire form of [`create_failed`], for a page that records it.
    fn create_failed_page(sequence: u64, branch: &str) -> serde_json::Value {
        json!({
            "id": format!("create-failed-{sequence}"),
            "sequence": sequence,
            "create_at": 2000,
            "type": "branch",
            "name": "create-failed",
            "data": {
                "status": "failed", "branch": branch,
                "createEvent": sequence - 1, "error": "creation refused"
            }
        })
    }

    /// A client whose daemon answered hello and then hung up: the next request
    /// fails where it is written, before any reply exists.
    async fn hung_up_client() -> Client {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let home = tempfile::tempdir().unwrap();
        let endpoint = home.path().join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (hung_up, wait) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _home = home;
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
                return;
            };
            let mut socket = reader.into_inner();
            let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":{
                "instanceId":"hung-up-test","serverVersion":"test","protocolVersion":4,
                "features":["session.create","session.history","session.history.latestTurn"],
                "daemonProfile":{"kind":"product"}
            }});
            if socket
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .is_err()
            {
                return;
            }
            let _ = socket.shutdown().await;
            drop(socket);
            let _ = hung_up.send(());
        });
        let client = Client::connect(&dim_client::ConnectionConfig {
            endpoint,
            server: dim_client::Server {
                program: std::env::current_exe().unwrap(),
                embedded: false,
            },
            profile: None,
            client_kind: "cli",
            client_name: "dim-cli-test",
            protocol_version: 4,
        })
        .await
        .unwrap();
        wait.await.unwrap();
        client
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

        worker.earlier("main");
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
        worker.earlier("main");
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

        worker.earlier("b-7c");
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

    /// A branch that ends while its older page is in flight does not finish
    /// its window on that page: the page is history the window has not read
    /// yet, not the branch's first window. Only a reply that answered no
    /// earlier read finishes an ended branch.
    #[tokio::test]
    async fn an_ended_branch_does_not_finish_on_an_earlier_page() {
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
        worker.earlier("b-7c");
        worker.poll_task_history().await.unwrap();
        {
            let history = worker.task_history.get_mut("b-7c").unwrap();
            history.done = false;
            history.ended = true;
        }
        let older: Vec<_> = (2..=101)
            .map(|sequence| text(sequence, &format!("r-older-{sequence}"), "older "))
            .collect();
        answer(
            &mut worker,
            json!({"events": older, "through": 300, "nextBefore": 2, "nextAfter": 101}),
        )
        .await;

        assert!(
            !worker.task_history["b-7c"].done,
            "an earlier page leaves the ended branch's window unfinished"
        );
        assert_eq!(
            worker.task_history["b-7c"].earlier,
            EarlierRead::None,
            "the earlier page settled the read it answered"
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
        worker.earlier("main");
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
        worker.earlier("main");
        assert!(
            worker.task_history_queue.is_empty(),
            "a press during the retried read queues nothing"
        );
    }

    /// A line whose first window read failed is not left waiting: opening the
    /// line again in `/lines` starts its window over, and the page that lands
    /// draws the conversation.
    #[tokio::test]
    async fn a_failed_line_window_reads_again_when_the_line_is_opened() {
        let (mut worker, mut receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.discover_branch("b-7c");
        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        fail(&mut worker, "the line's window").await;
        assert!(
            worker.lines["b-7c"].projection.rows().is_empty(),
            "a failed window has drawn nothing"
        );
        worker.publish();
        let snapshot = published(&mut receiver);
        assert!(
            snapshot
                .window_error
                .as_deref()
                .is_some_and(|error| error.starts_with("the daemon dropped the read")),
            "the failed window is published apart from a loading one: {:?}",
            snapshot.window_error
        );
        assert!(!snapshot.history_complete, "the window did not load");

        worker
            .command(crate::model::session::Command::TaskHistory {
                projection_id: worker.projection_id,
            })
            .await
            .unwrap();
        worker.publish();
        let snapshot = published(&mut receiver);
        assert!(
            snapshot.window_error.is_none(),
            "a retry is loading again, not still failed"
        );
        worker.poll_task_history().await.unwrap();
        fail(&mut worker, "the retried window").await;
        worker.publish();
        let snapshot = published(&mut receiver);
        assert!(
            snapshot.window_error.is_some(),
            "the retried window failed again"
        );
        worker.task_history_queue.clear();

        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        assert!(
            worker.task_history_request.is_some(),
            "opening the line again reads its window"
        );
        worker.task_history.get_mut("b-7c").unwrap().ended = true;
        answer(
            &mut worker,
            json!({"events":[text(300, "r-answer", "the answer")], "through": 300, "nextBefore": 300, "nextAfter": 300}),
        )
        .await;
        worker.poll_task_history().await.unwrap();

        let rows = worker.lines["b-7c"].projection.rows();
        assert_eq!(rows.len(), 1, "the page drew the line's conversation");
        assert_eq!(rows[0].text, "the answer");
        assert!(worker.task_history["b-7c"].done, "the read window finished");
        worker.publish();
        let snapshot = published(&mut receiver);
        assert!(
            snapshot.window_error.is_none(),
            "the read page cleared the failure"
        );
        assert!(snapshot.history_complete, "the window loaded");
        assert_eq!(snapshot.rows.len(), 1);
    }

    /// A failure after the window loaded belongs to that read: an inventory
    /// refresh or an `/earlier` page that fails leaves the loaded transcript
    /// drawn and the window complete.
    #[tokio::test]
    async fn late_inventory_and_earlier_failures_keep_the_loaded_line() {
        let (mut worker, mut receiver, _sent) =
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
            history.window_complete = true;
            history.window.through = Some(300);
            history.window.before = Some(200);
        }
        worker.apply("b-7c", event(300, "r-answer", "the loaded answer"));

        worker.task_history_queue.push_back("b-7c".into());
        worker.poll_task_history().await.unwrap();
        assert!(
            worker.task_history["b-7c"].error.is_some(),
            "the inventory read failed"
        );
        let rows = worker.lines["b-7c"].projection.rows();
        assert_eq!(rows.len(), 1, "the inventory failure drew over nothing");
        assert_eq!(rows[0].text, "the loaded answer");
        worker.publish();
        let snapshot = published(&mut receiver);
        assert!(
            snapshot.history_complete,
            "the loaded window stays complete"
        );
        assert_eq!(snapshot.rows.len(), 1, "the loaded row stays drawn");

        worker.earlier("b-7c");
        worker.poll_task_history().await.unwrap();
        fail(&mut worker, "the earlier page").await;
        assert_eq!(
            worker.lines["b-7c"].projection.rows().len(),
            1,
            "the failed older page drew over the loaded transcript"
        );
        assert_eq!(worker.earlier_view(), Earlier::Unread);
        worker.publish();
        let snapshot = published(&mut receiver);
        assert!(
            snapshot.history_complete,
            "a failed older page leaves the window complete"
        );
        assert_eq!(snapshot.rows.len(), 1, "the loaded row stays drawn");
    }

    /// A request that fails on the transport, before any reply exists, lands a
    /// first window in its failed state: the line shows the failure instead of
    /// staying blank and nothing reads it again on its own.
    #[tokio::test]
    async fn a_failed_window_send_lands_the_line_in_the_error_state() {
        let (mut worker, mut receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("main");
        worker.client = hung_up_client().await;

        worker.poll_task_history().await.unwrap();

        let history = &worker.task_history["main"];
        assert!(
            history.window_error.is_some(),
            "a window that could not be sent is a failed window: {:?}",
            history.error
        );
        assert!(!history.window_complete, "the window did not read");
        assert!(
            worker.task_history_request.is_none(),
            "no read is in flight"
        );
        assert!(
            !worker.task_history_queue.iter().any(|id| id == "main"),
            "the failed window is not read again on its own"
        );

        worker.publish();
        let snapshot = published(&mut receiver);
        assert!(
            snapshot.window_error.is_some(),
            "the failed window is published, not left loading"
        );
        assert!(!snapshot.history_complete, "the window did not load");
    }

    /// A first window that failed is not kept as the line's failure once its
    /// creation fails: the creation settles the line and `/tasks` no longer
    /// lists the read error, so a stale read failure would be all the line
    /// still showed.
    #[tokio::test]
    async fn a_create_failure_clears_a_failed_window() {
        let (mut worker, mut receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.discover_branch("b-7c");
        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        fail(&mut worker, "the line's window").await;
        assert!(worker.task_history["b-7c"].window_error.is_some());

        worker.task_event("b-7c", create_failed(400, "b-7c"));

        let history = &worker.task_history["b-7c"];
        assert!(history.done, "the failed creation settled the line");
        assert!(
            history.error.is_none(),
            "the tasks panel no longer lists it"
        );
        assert!(
            history.window_error.is_none(),
            "the line does not keep the read failure of a window it will never read"
        );
        assert!(
            !history.window_complete,
            "the failed creation read no window"
        );
        worker.publish();
        let snapshot = published(&mut receiver);
        assert!(
            snapshot.window_error.is_none(),
            "the settled line publishes no read failure"
        );
    }

    /// A creation that fails while the line's first window is still loading
    /// leaves no request that could settle it. The line is terminal, so the
    /// interface shows the line it was cut from, and `/lines` cannot show it
    /// again.
    #[tokio::test]
    async fn a_create_failure_before_the_first_window_moves_to_the_parent() {
        let (mut worker, mut receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("b-7c");
        worker.discover_branch("main");
        {
            let history = worker.task_history.get_mut("main").unwrap();
            history.done = true;
            history.window_complete = true;
            history.window.through = Some(1);
        }
        let create: Event = serde_json::from_value(json!({
            "id": "create", "sequence": 1, "create_at": 1000, "type": "branch", "name": "create",
            "data": {
                "at": {"kind": "sequence", "sequence": 0}, "branch": "b-7c", "prompt": "Check the parser",
                "source": {"kind": "host", "operation_id": "op"}
            }
        }))
        .expect("a branch create event");
        worker.discover_task_branches(&create);
        worker.apply("main", create);
        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        assert_eq!(
            worker
                .task_history_request
                .as_ref()
                .map(|read| read.branch_id.as_str()),
            Some("b-7c"),
            "the line's first window is in flight"
        );

        worker.task_event("main", create_failed(400, "b-7c"));

        assert_eq!(
            worker.focus, "main",
            "a line whose creation failed is not left loading"
        );
        let history = &worker.task_history["b-7c"];
        assert!(history.done, "the failed creation settled the line");
        assert!(
            !history.window_complete,
            "the failed creation read no window"
        );
        assert!(
            history.window_error.is_none(),
            "there is no first-window failure to show"
        );
        if let Some(read) = &worker.task_history_request {
            assert!(!read.current, "the failed line's read is no longer current");
        }
        assert!(
            !worker.task_history_queue.contains(&"b-7c".to_owned()),
            "the failed line is not read again on its own"
        );

        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        assert_eq!(
            worker.focus, "main",
            "the terminal line cannot be shown again"
        );
        let notices: Vec<String> = receiver
            .try_iter()
            .filter_map(|update| match update {
                Update::Notice(text) => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            notices
                .iter()
                .any(|notice| notice == "Line creation failed: b-7c"),
            "the refusal names the failed creation: {notices:?}"
        );

        worker.publish();
        let snapshot = published(&mut receiver);
        assert_eq!(snapshot.focus, "main", "the shown line survives");
        assert!(snapshot.window_error.is_none());
        assert!(snapshot.history_complete, "the surviving line is settled");
    }

    /// A creation that fails while the line's first window is still loading
    /// returns to the line it was cut from when the directory knows that
    /// line, not to `main`: the parent's window starts over and is read again.
    #[tokio::test]
    async fn a_create_failure_returns_to_a_parent_the_directory_knows() {
        let (mut worker, mut receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("b-7c");
        {
            let history = worker.task_history.get_mut("b-7c").unwrap();
            history.done = true;
            history.window_complete = true;
            history.window.through = Some(2);
        }
        worker.task_history_queue.clear();
        let create: Event = serde_json::from_value(json!({
            "id": "create-child", "sequence": 2, "create_at": 1000, "type": "branch", "name": "create",
            "data": {
                "at": {"kind": "sequence", "sequence": 1}, "branch": "b-7d", "prompt": "Narrow the parser",
                "source": {"kind": "host", "operation_id": "op"}
            }
        }))
        .expect("a branch create event");
        worker.apply("b-7c", create);
        assert!(
            worker
                .projection
                .line_views()
                .iter()
                .any(|line| line.id == "b-7d" && line.parent.as_deref() == Some("b-7c")),
            "the directory knows the line it was cut from"
        );
        assert!(
            !worker.lines.contains_key("b-7c"),
            "the parent's window is not open in the view"
        );
        worker
            .command(crate::model::session::Command::FocusLine("b-7d".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        assert_eq!(
            worker
                .task_history_request
                .as_ref()
                .map(|read| read.branch_id.as_str()),
            Some("b-7d"),
            "the line's first window is in flight"
        );

        worker.task_event("b-7c", create_failed(400, "b-7d"));

        assert_eq!(
            worker.focus, "b-7c",
            "a line whose creation failed returns to the line it was cut from"
        );
        let parent = &worker.task_history["b-7c"];
        assert_eq!(
            parent.window.through, None,
            "the returned-to window starts over"
        );
        assert!(
            !parent.window_complete,
            "the returned-to window is armed for a new read"
        );
        assert!(
            worker.task_history_queue.contains(&"b-7c".to_owned()),
            "the returned-to line is queued"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "the failed line is not read again on its own"
        );

        fail(&mut worker, "the failed line's first window").await;
        worker.poll_task_history().await.unwrap();
        assert_eq!(
            worker
                .task_history_request
                .as_ref()
                .map(|read| read.branch_id.as_str()),
            Some("b-7c"),
            "the returned-to line reads its window"
        );
        worker.task_history.get_mut("b-7c").unwrap().ended = true;
        answer(
            &mut worker,
            json!({"events":[text(300, "r-parent", "the parent's answer")], "through": 300, "nextBefore": 300, "nextAfter": 300}),
        )
        .await;
        worker.poll_task_history().await.unwrap();

        assert_eq!(
            worker.task_history["b-7c"].window.through,
            Some(300),
            "the returned-to window loaded its page"
        );
        assert!(
            worker.task_history["b-7c"].done,
            "the returned-to window finished"
        );
        let rows = worker.lines["b-7c"].projection.rows();
        assert_eq!(rows.len(), 1, "the page drew the parent's conversation");
        assert_eq!(rows[0].text, "the parent's answer");

        worker
            .command(crate::model::session::Command::FocusLine("b-7d".into()))
            .await
            .unwrap();
        assert_eq!(
            worker.focus, "b-7c",
            "the terminal line cannot be shown again"
        );
        let notices: Vec<String> = receiver
            .try_iter()
            .filter_map(|update| match update {
                Update::Notice(text) => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            notices
                .iter()
                .any(|notice| notice == "Line creation failed: b-7d"),
            "the refusal names the failed creation: {notices:?}"
        );

        worker.publish();
        let snapshot = published(&mut receiver);
        assert_eq!(snapshot.focus, "b-7c", "the shown line survives");
        assert!(
            snapshot.history_complete,
            "the returned-to line's window loaded"
        );
        assert!(snapshot.window_error.is_none());
    }

    /// A line cut from a line the directory knows, focused with its first
    /// read in flight, whose creation then fails: the parent's window starts
    /// over and the parent's read follows the failed line's last reply.
    async fn a_failed_creation_with_the_parent_window_reading()
    -> (Worker, std::sync::mpsc::Receiver<Update>) {
        let (mut worker, receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("b-7c");
        {
            let history = worker.task_history.get_mut("b-7c").unwrap();
            history.done = true;
            history.window_complete = true;
            history.window.through = Some(2);
        }
        worker.task_history_queue.clear();
        let create: Event = serde_json::from_value(json!({
            "id": "create-child", "sequence": 2, "create_at": 1000, "type": "branch", "name": "create",
            "data": {
                "at": {"kind": "sequence", "sequence": 1}, "branch": "b-7d", "prompt": "Narrow the parser",
                "source": {"kind": "host", "operation_id": "op"}
            }
        }))
        .expect("a branch create event");
        worker.apply("b-7c", create);
        worker
            .command(crate::model::session::Command::FocusLine("b-7d".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        assert_eq!(
            worker
                .task_history_request
                .as_ref()
                .map(|read| read.branch_id.as_str()),
            Some("b-7d"),
            "the line's first window is in flight"
        );

        worker.task_event("b-7c", create_failed(400, "b-7d"));
        assert_eq!(
            worker.focus, "b-7c",
            "a failed creation returns to the line it was cut from"
        );
        fail(&mut worker, "the failed line's first window").await;
        worker.poll_task_history().await.unwrap();
        assert_eq!(
            worker
                .task_history_request
                .as_ref()
                .map(|read| read.branch_id.as_str()),
            Some("b-7c"),
            "the parent's window is reading"
        );
        assert!(
            !worker.task_history["b-7c"].done,
            "the parent's window has not finished"
        );
        (worker, receiver)
    }

    /// A creation that fails while the line it was cut from is still reading
    /// its window is terminal for `/lines` at once: the directory records the
    /// failure when the fact arrives, not when the parent's window replays
    /// `live`.
    #[tokio::test]
    async fn a_create_failure_is_refused_before_the_parent_window_loads() {
        let (mut worker, receiver) = a_failed_creation_with_the_parent_window_reading().await;

        worker
            .command(crate::model::session::Command::FocusLine("b-7d".into()))
            .await
            .unwrap();

        assert_eq!(
            worker.focus, "b-7c",
            "the failed line is refused while its parent's window is still reading"
        );
        assert!(
            worker.task_history["b-7d"].done,
            "the refused line was not reset for a read"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "the refused line is not read again on its own"
        );
        let notices: Vec<String> = receiver
            .try_iter()
            .filter_map(|update| match update {
                Update::Notice(text) => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            notices
                .iter()
                .any(|notice| notice == "Line creation failed: b-7d"),
            "the refusal names the failed creation: {notices:?}"
        );
    }

    /// A parent's window that fails to read does not leave its failed creation
    /// selectable: the failure is a directory fact from the moment it arrives.
    #[tokio::test]
    async fn a_create_failure_is_still_refused_after_the_parent_window_fails() {
        let (mut worker, receiver) = a_failed_creation_with_the_parent_window_reading().await;

        fail(&mut worker, "the parent's window").await;
        worker.poll_task_history().await.unwrap();
        assert!(
            worker.task_history["b-7c"].window_error.is_some(),
            "the parent's window failed"
        );
        assert!(
            !worker.task_history["b-7c"].done,
            "the parent's window did not load"
        );

        worker
            .command(crate::model::session::Command::FocusLine("b-7d".into()))
            .await
            .unwrap();

        assert_eq!(
            worker.focus, "b-7c",
            "the failed line is refused after its parent's window failed"
        );
        assert!(
            worker.task_history["b-7d"].done,
            "the refused line was not reset for a read"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "the refused line is not read again on its own"
        );
        let notices: Vec<String> = receiver
            .try_iter()
            .filter_map(|update| match update {
                Update::Notice(text) => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            notices
                .iter()
                .any(|notice| notice == "Line creation failed: b-7d"),
            "the refusal names the failed creation: {notices:?}"
        );
    }

    /// A live failure the parent's page has not projected a `branch/create`
    /// for, with the parent's inventory still in flight: the child row is
    /// projected terminal, `/lines` refuses it for the whole gap, and neither
    /// the refusal nor the parent's finished window resets or queues it.
    #[tokio::test]
    async fn a_create_failure_before_its_create_is_projected_is_terminal() {
        let (mut worker, mut receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("b-7c");
        worker
            .command(Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        assert_eq!(
            worker
                .task_history_request
                .as_ref()
                .map(|read| read.branch_id.as_str()),
            Some("b-7c"),
            "the parent's first window is in flight"
        );

        worker.task_event("b-7c", create_failed(400, "b-7d"));
        answer(
            &mut worker,
            json!({"events":[{
                "id": "create-child", "sequence": 2, "create_at": 1000, "type": "branch", "name": "create",
                "data": {
                    "at": {"kind": "sequence", "sequence": 1}, "branch": "b-7d", "prompt": "Narrow the parser",
                    "source": {"kind": "host", "operation_id": "op"}
                }
            }], "through": 300, "nextBefore": 1, "nextAfter": 300}),
        )
        .await;

        worker.methods.insert("session.input.pending".into());
        worker.methods.insert("session.turn.get".into());
        worker.poll_task_history().await.unwrap();
        assert!(
            matches!(
                worker.task_history_request.as_ref().map(|read| &read.stage),
                Some(ReadStage::Inventory("session.input.pending"))
            ),
            "the parent's inventory is gated"
        );

        let child = worker
            .projection
            .line_views()
            .iter()
            .find(|line| line.id == "b-7d")
            .expect("the parent's page projected the child");
        assert_eq!(
            child.state,
            LineState::CreationFailed,
            "the projected child is terminal, not waiting"
        );
        worker
            .command(Command::FocusLine("b-7d".into()))
            .await
            .unwrap();
        assert_eq!(
            worker.focus, "b-7c",
            "the child is refused while its parent's inventory is in flight"
        );
        assert!(
            worker.task_history["b-7d"].done,
            "the refused child was not reset for a read"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "the refused child is not queued for a read"
        );
        assert!(
            !worker.lines.contains_key("b-7d"),
            "the refused child has no transcript"
        );
        let failed_notice = |receiver: &mut std::sync::mpsc::Receiver<Update>| -> usize {
            receiver
                .try_iter()
                .filter_map(|update| match update {
                    Update::Notice(text) => Some(text),
                    _ => None,
                })
                .filter(|notice| notice == "Branch b-7d: creation refused")
                .count()
        };
        assert_eq!(
            failed_notice(&mut receiver),
            1,
            "the failure is announced once"
        );

        answer(
            &mut worker,
            json!({"branchId":"b-7c","through":300,"inputs":[],"next":null}),
        )
        .await;
        worker.poll_task_history().await.unwrap();
        answer(
            &mut worker,
            json!({"branchId":"b-7c","through":300,"turn":null,"output":null}),
        )
        .await;
        worker.poll_task_history().await.unwrap();
        assert!(
            worker.task_history["b-7c"].done,
            "the parent's window finished"
        );
        assert!(
            worker.task_history["b-7d"].done,
            "the parent's window did not reset the child"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "the parent's window did not queue the child"
        );
        assert_eq!(
            failed_notice(&mut receiver),
            0,
            "the drained failure is not announced again"
        );

        worker
            .command(Command::TaskHistory {
                projection_id: worker.projection_id,
            })
            .await
            .unwrap();
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "reopening /tasks does not queue the child"
        );
        worker
            .command(Command::FocusLine("b-7d".into()))
            .await
            .unwrap();
        assert_eq!(worker.focus, "b-7c", "the child stays refused");

        receiver.try_iter().for_each(drop);
        worker.invalidate_projection();
        assert!(
            worker.projection.line_views().is_empty(),
            "a new Session's projection holds no line from the old one"
        );
        worker
            .command(Command::FocusLine("b-7d".into()))
            .await
            .unwrap();
        let notices: Vec<String> = receiver
            .try_iter()
            .filter_map(|update| match update {
                Update::Notice(text) => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            notices
                .iter()
                .any(|notice| notice == "No line b-7d in this session"),
            "a re-attached Session does not know the failed line: {notices:?}"
        );
        assert!(
            !notices
                .iter()
                .any(|notice| notice == "Line creation failed: b-7d"),
            "the old Session's failure did not cross over: {notices:?}"
        );
        assert_eq!(
            worker.focus, "main",
            "a re-attached Session does not know the failed line"
        );
    }

    /// A page that records the child's creation and its failure in one read:
    /// the replay folds the pair before any snapshot goes out, so the child is
    /// terminal from that page alone, and a failed inventory on the parent
    /// changes neither the child's state nor the queue.
    #[tokio::test]
    async fn a_page_that_records_a_create_and_its_failure_leaves_the_line_terminal() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("b-7c");
        worker
            .command(Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        assert_eq!(
            worker
                .task_history_request
                .as_ref()
                .map(|read| read.branch_id.as_str()),
            Some("b-7c"),
            "the parent's first window is in flight"
        );

        answer(
            &mut worker,
            json!({"events":[{
                "id": "create-child", "sequence": 2, "create_at": 1000, "type": "branch", "name": "create",
                "data": {
                    "at": {"kind": "sequence", "sequence": 1}, "branch": "b-7d", "prompt": "Narrow the parser",
                    "source": {"kind": "host", "operation_id": "op"}
                }
            }, create_failed_page(3, "b-7d")], "through": 300, "nextBefore": 1, "nextAfter": 300}),
        )
        .await;

        let child = worker
            .projection
            .line_views()
            .iter()
            .find(|line| line.id == "b-7d")
            .expect("the page projected the child");
        assert_eq!(
            child.state,
            LineState::CreationFailed,
            "the page's failure is the child's only projected state"
        );
        worker
            .command(Command::FocusLine("b-7d".into()))
            .await
            .unwrap();
        assert_eq!(worker.focus, "b-7c", "the child is refused");
        assert!(
            worker.task_history["b-7d"].done,
            "the refused child was not reset for a read"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "the refused child is not queued for a read"
        );
        assert!(
            !worker.lines.contains_key("b-7d"),
            "the refused child has no transcript"
        );

        worker.methods.insert("session.input.pending".into());
        worker.methods.insert("session.turn.get".into());
        worker.poll_task_history().await.unwrap();
        fail(&mut worker, "the parent's inventory").await;
        worker.poll_task_history().await.unwrap();
        assert!(
            worker.task_history["b-7c"].error.is_some(),
            "the parent's inventory failed"
        );
        assert!(
            worker.task_history["b-7d"].done,
            "the failed inventory left the child settled"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "the failed inventory did not queue the child"
        );

        worker
            .command(Command::TaskHistory {
                projection_id: worker.projection_id,
            })
            .await
            .unwrap();
        assert!(
            worker.task_history_queue.contains(&"b-7c".to_owned()),
            "reopening /tasks retries the parent"
        );
        assert!(
            !worker.task_history_queue.contains(&"b-7d".to_owned()),
            "reopening /tasks does not queue the child"
        );
        worker
            .command(Command::FocusLine("b-7d".into()))
            .await
            .unwrap();
        assert_eq!(worker.focus, "b-7c", "the child stays refused");
    }

    /// A child its parent's first page published as waiting, whose creation
    /// fails after the parent's inventory failed while the parent stays
    /// focused: the terminal directory state publishes a snapshot on its own,
    /// and a repeated failure schedules nothing.
    #[tokio::test]
    async fn a_live_create_failure_publishes_the_terminal_directory_without_focus() {
        let (mut worker, mut receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = "main".into();
        worker.discover_branch("b-7c");
        worker
            .command(Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        answer(
            &mut worker,
            json!({"events":[{
                "id": "create-child", "sequence": 2, "create_at": 1000, "type": "branch", "name": "create",
                "data": {
                    "at": {"kind": "sequence", "sequence": 1}, "branch": "b-7d", "prompt": "Narrow the parser",
                    "source": {"kind": "host", "operation_id": "op"}
                }
            }], "through": 300, "nextBefore": 1, "nextAfter": 300}),
        )
        .await;
        let snapshot = published(&mut receiver);
        let child = snapshot
            .lines
            .iter()
            .find(|line| line.id == "b-7d")
            .expect("the parent's first page published the child");
        assert_eq!(
            child.state,
            LineState::Waiting,
            "the child waits for its creation"
        );

        worker.methods.insert("session.input.pending".into());
        worker.methods.insert("session.turn.get".into());
        worker.task_history_queue.clear();
        worker.task_history_queue.push_back("b-7c".into());
        worker.poll_task_history().await.unwrap();
        assert!(
            matches!(
                worker.task_history_request.as_ref().map(|read| &read.stage),
                Some(ReadStage::Inventory("session.input.pending"))
            ),
            "the parent's inventory is in flight"
        );
        fail(&mut worker, "the parent's inventory").await;
        assert!(
            worker.task_history["b-7c"].error.is_some(),
            "the parent's inventory failed"
        );
        worker.publish();
        let snapshot = published(&mut receiver);
        let child = snapshot
            .lines
            .iter()
            .find(|line| line.id == "b-7d")
            .expect("the failed inventory leaves the child listed");
        assert_eq!(
            child.state,
            LineState::Waiting,
            "the failed inventory left the child waiting"
        );
        assert_eq!(snapshot.focus, "b-7c", "the parent stays shown");

        worker.task_event("b-7c", create_failed(400, "b-7d"));
        assert!(
            worker.publish_at.is_some(),
            "the terminal directory state schedules a snapshot without focus"
        );
        worker.publish();
        let snapshot = published(&mut receiver);
        let child = snapshot
            .lines
            .iter()
            .find(|line| line.id == "b-7d")
            .expect("the published directory keeps the child");
        assert_eq!(
            child.state,
            LineState::CreationFailed,
            "the published snapshot shows the terminal state, not waiting"
        );
        assert_eq!(snapshot.focus, "b-7c", "the parent stays shown");

        worker.task_event("b-7c", create_failed(401, "b-7d"));
        assert!(
            worker.publish_at.is_none(),
            "a repeated failure schedules no second snapshot"
        );
    }

    fn published(receiver: &mut std::sync::mpsc::Receiver<Update>) -> ViewSnapshot {
        let mut snapshot = None;
        while let Ok(update) = receiver.try_recv() {
            if let Update::Snapshot(state) = update {
                snapshot = Some(*state);
            }
        }
        snapshot.expect("a published snapshot")
    }

    /// A line's own window can start after its start. The lines it opened show
    /// no count then, and the earlier page that reaches the start fills it in.
    #[tokio::test]
    async fn an_earlier_page_of_a_line_counts_the_lines_it_opened() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.discover_branch("main");
        {
            let history = worker.task_history.get_mut("main").unwrap();
            history.done = true;
            history.window.through = Some(4);
        }
        worker.apply("main", tell(said(2, "find the leak")));
        worker.apply("main", tell(line_created(4, "b-7c", 2)));
        assert_eq!(
            listed(&worker, "b-7c").cut,
            Some(crate::model::view::LineCut {
                sequence: 2,
                messages: 1,
            }),
            "the line b-7c is cut after main's first message"
        );

        worker.task_history_queue.clear();
        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.task_history_queue.push_back("b-7c".into());
        worker.poll_task_history().await.unwrap();
        answer(
            &mut worker,
            json!({
                "events": [said(10, "carry on"), line_created(12, "b-90", 10)],
                "through": 12, "nextBefore": 10, "nextAfter": 12,
            }),
        )
        .await;
        worker.task_history_queue.clear();
        worker.task_history.get_mut("b-7c").unwrap().done = true;
        assert_eq!(
            listed(&worker, "b-90").cut,
            None,
            "the line's window starts after its start, so the cut counts nothing"
        );

        worker.earlier("b-7c");
        assert_eq!(worker.earlier_view(), Earlier::Reading);
        worker.poll_task_history().await.unwrap();
        answer(
            &mut worker,
            json!({
                "events": [said(5, "the first ask")],
                "through": 12, "nextBefore": 1, "nextAfter": 5,
            }),
        )
        .await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 10,
                messages: 3,
            }),
            "the earlier page reaches the line's start and the cut counts every message"
        );
    }

    /// The directory's row for `branch`, a line of `parent` cut at `at`.
    fn listed_line(branch: &str, parent: &str, at: u64) -> dim_client::wire::BranchEntry {
        dim_client::wire::BranchEntry {
            purpose: Some(branch.into()),
            cut_messages: None,
            end: None,
            branch_id: branch.into(),
            parent_branch_id: Some(parent.into()),
            branch_point: Some(at),
            state: "open".into(),
            head: 0,
        }
    }

    /// A Session whose main history is whole and cut `b-7c` after its first
    /// message, and a `session.turn.get` read that moved the Session cursor to
    /// the head of `b-7c` without replaying an event of its history. `b-90` was
    /// created before this attachment read `b-7c`, so only the directory lists it.
    async fn a_line_discovered_through_its_cut(worker: &mut Worker) {
        worker.session_id = "s1".into();
        worker.discover_branch("main");
        {
            let history = worker.task_history.get_mut("main").unwrap();
            history.done = true;
            history.window.through = Some(4);
        }
        worker.apply("main", tell(said(2, "find the leak")));
        worker.apply("main", tell(line_created(4, "b-7c", 2)));
        worker.projection.directory(&listed_line("b-7c", "main", 2));
        worker
            .projection
            .directory(&listed_line("b-90", "b-7c", 10));

        worker.task_history_queue.clear();
        worker.task_history_queue.push_back("b-7c".into());
        worker.poll_task_history().await.unwrap();
        answer(
            worker,
            json!({"branchId":"b-7c","through":12,"turn":null,"output":null}),
        )
        .await;
        worker.task_history_queue.clear();
        worker.finish_window("b-7c");
        assert_eq!(
            worker.cursors.get("b-7c"),
            Some(&12),
            "the finished cut read left the Session cursor at the line's head"
        );
    }

    /// Opens `branch` and answers the first page its history read asks for.
    async fn open_line(worker: &mut Worker, branch: &str, page: serde_json::Value) {
        worker.task_history_queue.clear();
        worker
            .command(crate::model::session::Command::FocusLine(branch.into()))
            .await
            .unwrap();
        worker.poll_task_history().await.unwrap();
        answer(worker, page).await;
        worker.task_history_queue.clear();
    }

    /// A cut read moves the Session cursor to a line's head without replaying
    /// its events. Opening the line replays its history below that cursor, and
    /// the line list must still count it: the child the line created before this
    /// attachment, and the child it opens afterwards, show the count of the
    /// whole prefix, and the same page again counts nothing twice.
    #[tokio::test]
    async fn opening_a_line_counts_the_history_the_cursor_passed() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        a_line_discovered_through_its_cut(&mut worker).await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            None,
            "the cut read leaves the line's child without a count"
        );

        open_line(
            &mut worker,
            "b-7c",
            json!({
                "events": [
                    said(5, "the first ask"),
                    said(10, "carry on"),
                    line_created(12, "b-90", 10),
                ],
                "through": 12, "nextBefore": 1, "nextAfter": 12,
            }),
        )
        .await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 10,
                messages: 3,
            }),
            "opening the line counts the child it created before the window"
        );

        worker.task_history.get_mut("b-7c").unwrap().done = true;
        worker.apply("b-7c", tell(said(13, "next ask")));
        worker.apply("b-7c", tell(line_created(15, "b-91", 14)));
        assert_eq!(
            listed(&worker, "b-91").cut,
            Some(crate::model::view::LineCut {
                sequence: 14,
                messages: 4,
            }),
            "the line counts every message it holds when it opens a child"
        );

        for event in [
            said(5, "the first ask"),
            said(10, "carry on"),
            line_created(12, "b-90", 10),
        ] {
            worker.replay("b-7c", tell(event));
        }
        assert_eq!(
            listed(&worker, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 10,
                messages: 3,
            }),
            "the same page again counts nothing twice"
        );
    }

    /// A line opened before its history reaches its start shows no count for the
    /// lines it opened, not a smaller one; the earlier page that reaches the
    /// start completes the count.
    #[tokio::test]
    async fn a_line_opened_before_its_start_counts_after_the_earlier_page() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        a_line_discovered_through_its_cut(&mut worker).await;
        open_line(
            &mut worker,
            "b-7c",
            json!({
                "events": [said(10, "carry on"), line_created(12, "b-90", 10)],
                "through": 12, "nextBefore": 10, "nextAfter": 12,
            }),
        )
        .await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            None,
            "the window starts after the line's own start, so its cut counts nothing"
        );

        worker.task_history.get_mut("b-7c").unwrap().done = true;
        worker.earlier("b-7c");
        assert_eq!(worker.earlier_view(), Earlier::Reading);
        worker.poll_task_history().await.unwrap();
        answer(
            &mut worker,
            json!({
                "events": [said(4, "the first ask")],
                "through": 12, "nextBefore": 1, "nextAfter": 4,
            }),
        )
        .await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 10,
                messages: 3,
            }),
            "the earlier page reaches the start and counts what the cut inherits"
        );
    }

    /// The line list counts the same whichever of a line's open page and an
    /// earlier page arrives first, and counts nothing twice.
    #[tokio::test]
    async fn a_line_counts_the_same_whichever_page_arrives_first() {
        let page = || {
            json!({
                "events": [
                    said(4, "the first ask"),
                    said(10, "carry on"),
                    line_created(12, "b-90", 10),
                ],
                "through": 12, "nextBefore": 1, "nextAfter": 12,
            })
        };

        let (mut earlier_first, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        a_line_discovered_through_its_cut(&mut earlier_first).await;
        earlier_first
            .projection
            .line_event("b-7c", &tell(said(4, "the first ask")));
        open_line(&mut earlier_first, "b-7c", page()).await;
        assert_eq!(
            listed(&earlier_first, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 10,
                messages: 3,
            }),
            "a page read earlier counts once when the open page repeats it"
        );

        let (mut open_first, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        a_line_discovered_through_its_cut(&mut open_first).await;
        open_line(&mut open_first, "b-7c", page()).await;
        open_first
            .projection
            .line_event("b-7c", &tell(said(4, "the first ask")));
        assert_eq!(
            listed(&open_first, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 10,
                messages: 3,
            }),
            "an earlier page that repeats the open page counts nothing twice"
        );
    }

    /// The open page carries the withdrawal and a cut after it while the
    /// withdrawn input sits on an earlier page: the cut after the withdrawal
    /// leaves the input out, and a cut before it keeps counting the input.
    #[tokio::test]
    async fn a_withdrawal_below_the_cursor_keeps_each_cut_at_its_own_sequence() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        a_line_discovered_through_its_cut(&mut worker).await;
        worker.projection.directory(&listed_line("b-91", "b-7c", 7));
        assert_eq!(listed(&worker, "b-90").cut, None);
        assert_eq!(listed(&worker, "b-91").cut, None);

        open_line(
            &mut worker,
            "b-7c",
            json!({
                "events": [
                    input_deleted(9, "in-5"),
                    text(10, "r1", "carry on"),
                    line_created(12, "b-90", 10),
                ],
                "through": 12, "nextBefore": 9, "nextAfter": 12,
            }),
        )
        .await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            None,
            "the window starts after the line's own start, so its cut counts nothing"
        );

        worker.task_history.get_mut("b-7c").unwrap().done = true;
        worker.earlier("b-7c");
        worker.poll_task_history().await.unwrap();
        answer(
            &mut worker,
            json!({
                "events": [said(5, "withdrawn ask"), line_created(8, "b-91", 7)],
                "through": 12, "nextBefore": 1, "nextAfter": 8,
            }),
        )
        .await;
        assert_eq!(
            listed(&worker, "b-91").cut,
            Some(crate::model::view::LineCut {
                sequence: 7,
                messages: 2,
            }),
            "a withdrawal after the cut leaves its message in the count"
        );
        assert_eq!(
            listed(&worker, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 10,
                messages: 1,
            }),
            "a withdrawal before the cut leaves its message out of the count"
        );
    }

    /// A line read only through `session.turn.get` never loaded its history:
    /// the cuts that count it show nothing, even when live events already
    /// counted them.
    #[tokio::test]
    async fn a_line_read_only_through_its_cut_counts_nothing() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.discover_branch("main");
        {
            let history = worker.task_history.get_mut("main").unwrap();
            history.done = true;
            history.window.through = Some(4);
        }
        worker.apply("main", tell(said(2, "find the leak")));
        worker.apply("main", tell(line_created(4, "b-7c", 2)));

        worker.task_history_queue.clear();
        worker.task_history_queue.push_back("b-7c".into());
        worker.poll_task_history().await.unwrap();
        worker.apply("b-7c", tell(said(10, "carry on")));
        worker.apply("b-7c", tell(line_created(12, "b-90", 10)));
        assert_eq!(
            listed(&worker, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 10,
                messages: 2,
            }),
            "the live events counted the cut before the read settled"
        );

        answer(
            &mut worker,
            json!({"branchId":"b-7c","through":12,"turn":null,"output":null}),
        )
        .await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            None,
            "the line was never read from its start, so its cut counts nothing"
        );
    }

    /// The methods a test worker offers when its history reads use display
    /// transcript pages, whose `done` flag says the page reached the start.
    const TRANSCRIPT_FEATURES: [&str; 7] = [
        "session.create",
        "session.history",
        "session.history.latestTurn",
        "session.input.pending.blockPresence",
        "session.history.display",
        "session.history.detail",
        "session.history.display.transcript",
    ];

    /// A display transcript page carrying `items` and the cursor below them.
    fn transcript_page(
        items: Vec<serde_json::Value>,
        next_before: u64,
        done: bool,
    ) -> serde_json::Value {
        json!({
            "items": items, "through": 1000, "nextBefore": next_before,
            "nextAfter": 1000, "done": done,
        })
    }

    /// `count` display-only text events from `first`; each request id is its
    /// own transcript row.
    fn draft_page(first: u64, count: u64) -> Vec<serde_json::Value> {
        (first..first + count)
            .map(|sequence| text(sequence, &format!("r{sequence}"), "draft "))
            .collect()
    }

    /// `count` user messages from `first`, each its own transcript row and each
    /// one a message a cut counts.
    fn asked_page(first: u64, count: u64) -> Vec<serde_json::Value> {
        (first..first + count)
            .map(|sequence| said(sequence, &format!("ask {sequence}")))
            .collect()
    }

    /// Opens `b-7c` in a transcript window that starts after its own start,
    /// then answers an `/earlier` read whose pages carry 10, 20, 30 and 250
    /// rows. The retention budget keeps the newest 300 rows, so the oldest
    /// page is dropped even though its `done` flag reached the line's start;
    /// that page carries every user message `b-90` inherits.
    async fn a_transcript_read_that_drops_its_oldest_page() -> Worker {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&TRANSCRIPT_FEATURES).await;
        worker.session_id = "s1".into();
        worker.discover_branch("main");
        {
            let history = worker.task_history.get_mut("main").unwrap();
            history.done = true;
            history.window.through = Some(4);
        }
        worker.apply("main", tell(said(2, "find the leak")));
        worker.apply("main", tell(line_created(4, "b-7c", 2)));

        let mut head = draft_page(331, 10);
        head.push(line_created(341, "b-90", 240));
        worker.task_history_queue.clear();
        worker
            .command(crate::model::session::Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        worker.task_history_queue.push_back("b-7c".into());
        worker.poll_task_history().await.unwrap();
        answer(&mut worker, transcript_page(head, 331, false)).await;
        worker.task_history_queue.clear();
        worker.task_history.get_mut("b-7c").unwrap().done = true;

        worker.earlier("b-7c");
        for (first, count) in [(321, 10), (301, 20), (271, 30)] {
            worker.poll_task_history().await.unwrap();
            answer(
                &mut worker,
                transcript_page(draft_page(first, count), first, false),
            )
            .await;
        }
        worker.poll_task_history().await.unwrap();
        answer(&mut worker, transcript_page(asked_page(11, 250), 11, true)).await;
        worker
    }

    /// The page that reached the line's start is dropped under the retention
    /// budget, so the retained history does not cover the start: the cut stays
    /// unknown instead of counting the messages that happened to survive.
    #[tokio::test]
    async fn a_history_read_that_drops_its_oldest_page_leaves_the_cut_unknown() {
        let worker = a_transcript_read_that_drops_its_oldest_page().await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            None,
            "the retained history misses the dropped page, so the count is unknown"
        );
    }

    /// The dropped page loaded on a later `/earlier` fills the gap: the cut is
    /// recalculated from the whole history, equal to a complete read.
    #[tokio::test]
    async fn loading_the_page_a_retention_drop_omitted_recounts_the_cut() {
        let mut worker = a_transcript_read_that_drops_its_oldest_page().await;
        assert_eq!(
            worker.earlier_view(),
            Earlier::Unread,
            "the dropped page is the earlier history left to read"
        );
        worker.earlier("b-7c");
        worker.poll_task_history().await.unwrap();
        answer(&mut worker, transcript_page(asked_page(11, 250), 11, true)).await;
        assert_eq!(
            listed(&worker, "b-90").cut,
            Some(crate::model::view::LineCut {
                sequence: 240,
                messages: 231,
            }),
            "the page that fills the gap counts what a complete read counts"
        );
    }

    /// A window whose page left out earlier content says so in a notice. The
    /// notice is sent after the snapshot that carries the page, so the
    /// interface's next frame draws the page's rows before the notice that
    /// reports them missing. Both updates share one channel, and the interface
    /// applies whatever it drains in that order.
    #[tokio::test]
    async fn a_missing_prefix_notice_follows_the_page_it_reports() {
        let (mut worker, receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        worker.session_id = "s1".into();
        worker.focus = DEFAULT_BRANCH_ID.into();
        worker.discover_branch(DEFAULT_BRANCH_ID);
        worker.poll_task_history().await.unwrap();
        answer(
            &mut worker,
            json!({
                "events": [text(2, "r-loaded", "the loaded page")],
                "through": 2, "nextAfter": 2, "nextBefore": 2
            }),
        )
        .await;

        let updates: Vec<Update> = receiver.try_iter().collect();
        let page = updates
            .iter()
            .position(|update| {
                matches!(update, Update::Snapshot(view)
                    if view.rows.iter().any(|row| row.text.contains("the loaded page")))
            })
            .expect("the page published its rows");
        let notice = updates
            .iter()
            .position(|update| {
                matches!(update, Update::Notice(text)
                    if text == "Earlier content not loaded; /earlier to read it")
            })
            .expect("the page reports the earlier content it left out");
        assert!(
            page < notice,
            "the page's snapshot is sent before the notice that reports the missing earlier content"
        );
    }
}

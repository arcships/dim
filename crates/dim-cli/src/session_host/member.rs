//! Read-only member observations on the leader's existing connection: one
//! per member Session that an unfinished delegation card or the selected
//! detail needs, shared by both. History pages go through one slot.

use std::collections::{HashMap, HashSet, VecDeque};

use super::*;
use crate::model::view::{Earlier, MemberConversationState};

pub(super) enum Notice {
    Event(Event),
    Progress(Progress),
    State(bool),
}

enum Pending {
    Attach(u64),
    History(u64),
    Detach(u64),
    Turn(u64),
    Methods(u64),
    Goal(u64),
    Tool(u64, String, String),
}

/// The earlier read the observation was asked for: none, a press taken while
/// another request was in flight, or the read a press sent.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum EarlierRead {
    #[default]
    None,
    Asked,
    Reading,
}

struct Observation {
    /// The leader attachment it was made for; a replacement releases it.
    projection_id: u64,
    projection: Projection,
    state: MemberConversationState,
    /// The latest `context/configured` the member adopted.
    model: Option<String>,
    attached: bool,
    closing: bool,
    pending: Option<Pending>,
    window: dim_client::window::Window,
    model_through: u64,
    earlier: EarlierRead,
    goal_available: bool,
    tool_available: bool,
    goal_stale: bool,
    cursor: u64,
    live: Vec<Notice>,
}

impl Observation {
    fn apply(&mut self, notice: Notice) {
        match notice {
            Notice::Event(event) => self.event(event, false),
            Notice::Progress(progress) => self.projection.progress(&progress),
            Notice::State(running) => self.projection.set_running(running),
        }
    }

    /// Folds one event of the member's Session; `replayed` says a read of its
    /// recorded history carried it, as every page of the conversation does.
    fn event(&mut self, event: Event, replayed: bool) {
        if let EventContent::Configuration(configuration) = &event.content
            && event.sequence > self.model_through
        {
            self.model_through = event.sequence;
            self.model = Some(format!(
                "{} / {}",
                configuration.provider_id, configuration.model_id
            ));
        }
        if event.sequence > self.cursor {
            self.cursor = event.sequence;
            self.goal_stale |= crate::goal::changed(&event.content);
            match replayed {
                true => self.projection.replay("main", &event),
                false => self.projection.apply("main", &event),
            }
        }
    }

    /// Ends the observation: it read nothing and has nothing left to read.
    fn release(&mut self) {
        self.closing = true;
        self.window = Default::default();
        self.live.clear();
        self.earlier = EarlierRead::None;
    }
}

#[derive(Default)]
pub(super) struct Members {
    observations: HashMap<String, Observation>,
    /// The detail's member: the attachment it was opened on and its Session.
    pub(super) selected: Option<(u64, String)>,
    /// Observations waiting for the history-page slot, oldest first.
    pages: VecDeque<String>,
    /// Members whose read failed or whose Session closed: an unfinished card
    /// does not reattach them; opening the member again does.
    unavailable: HashSet<String>,
    /// The `agent.list` read in flight, with the attachment and the Agent
    /// ids it names.
    agents: Option<(u64, u64, Vec<String>)>,
}

impl Members {
    pub(super) fn owns(&self, request_id: u64) -> bool {
        self.agents
            .as_ref()
            .is_some_and(|(id, _, _)| *id == request_id)
            || self.request(request_id).is_some()
    }

    fn request(&self, request_id: u64) -> Option<String> {
        self.observations.iter().find_map(|(id, observation)| {
            matches!(
                observation.pending,
                Some(Pending::Attach(at) | Pending::History(at) | Pending::Detach(at) | Pending::Turn(at) | Pending::Methods(at) | Pending::Goal(at) | Pending::Tool(at, _, _)) if at == request_id
            )
            .then(|| id.clone())
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentSummary {
    agent_id: String,
    name: String,
}

impl Worker {
    pub(super) fn member_projection(&mut self, session_id: &str) -> Option<&mut Projection> {
        self.members
            .observations
            .get_mut(session_id)
            .filter(|observation| {
                !observation.closing && observation.projection_id == self.projection_id
            })
            .map(|observation| &mut observation.projection)
    }

    pub(super) fn watch_member(&mut self, member_id: Option<String>) {
        let selected = member_id.map(|id| (self.projection_id, id));
        if selected == self.members.selected {
            return;
        }
        if let Some((_, id)) = &selected {
            self.members.unavailable.remove(id);
        }
        self.members.selected = selected;
        self.publish_member();
    }

    /// A replacement leader releases every observation the old one owned.
    pub(super) fn release_members(&mut self) {
        self.members.selected = None;
        self.members.unavailable.clear();
        self.members.pages.clear();
    }

    pub(super) fn observes_member(&self, session_id: &str) -> bool {
        self.members
            .observations
            .get(session_id)
            .is_some_and(|observation| !observation.closing)
    }

    /// Whether the selected member's conversation has older history to read,
    /// by the fields `earlier_member` tests.
    pub(super) fn earlier_observation(&self) -> Earlier {
        let Some((_, id)) = &self.members.selected else {
            return Earlier::AtStart;
        };
        self.members
            .observations
            .get(id)
            .map_or(Earlier::AtStart, |observation| {
                if observation.earlier != EarlierRead::None {
                    Earlier::Reading
                } else if observation.window.before.is_some() {
                    Earlier::Unread
                } else {
                    Earlier::AtStart
                }
            })
    }

    /// Unfinished delegations' members, once main's history says which are
    /// unfinished, and the selected detail's member.
    fn needed_members(&self) -> HashSet<String> {
        let mut needed = HashSet::new();
        if self
            .task_history
            .get("main")
            .is_some_and(|branch| branch.done)
        {
            needed.extend(self.projection.unfinished_members().map(str::to_owned));
        }
        if let Some((projection_id, id)) = &self.members.selected
            && *projection_id == self.projection_id
        {
            needed.insert(id.clone());
        }
        needed.retain(|id| !self.members.unavailable.contains(id));
        needed
    }

    pub(super) async fn poll_member(&mut self) -> crate::Result<()> {
        self.poll_agents().await?;
        let needed = self.needed_members();
        for (id, observation) in &mut self.members.observations {
            if !needed.contains(id) || observation.projection_id != self.projection_id {
                observation.release();
            }
        }
        for id in needed {
            if !self.members.observations.contains_key(&id) {
                self.members.observations.insert(
                    id.clone(),
                    Observation {
                        projection_id: self.projection_id,
                        projection: Projection::default(),
                        state: MemberConversationState::Loading,
                        model: None,
                        attached: false,
                        closing: false,
                        pending: None,
                        window: Default::default(),
                        model_through: 0,
                        earlier: EarlierRead::None,
                        goal_available: false,
                        tool_available: false,
                        goal_stale: false,
                        cursor: 0,
                        live: Vec::new(),
                    },
                );
                self.member_changed(&id);
            }
        }
        let ids: Vec<String> = self.members.observations.keys().cloned().collect();
        for id in ids {
            let observation = &self.members.observations[&id];
            if observation.pending.is_some() {
                continue;
            }
            let missing = (self
                .members
                .selected
                .as_ref()
                .is_some_and(|(_, selected)| selected == &id)
                && observation.state == MemberConversationState::Live
                && observation.tool_available)
                .then(|| observation.projection.missing_tool())
                .flatten();
            let method = if observation.closing && !observation.attached {
                self.members.observations.remove(&id);
                continue;
            } else if observation.closing {
                "session.detach"
            } else if !observation.attached {
                "session.attach"
            } else if observation.state == MemberConversationState::Live
                && observation.goal_available
                && observation.goal_stale
            {
                "session.goal.get"
            } else if missing.is_some() {
                "session.tool.get"
            } else {
                continue;
            };
            let mut params = json!({"sessionId":id});
            if method == "session.attach"
                && self
                    .client
                    .supports(&["session.history.display", "session.history.detail"])
            {
                params["display"] = json!(true);
            }
            if method == "session.goal.get" {
                params["branchId"] = json!("main");
            }
            if method == "session.tool.get" {
                let (request, call) = missing.as_ref().unwrap();
                params["branchId"] = json!("main");
                params["requestId"] = json!(request);
                params["callId"] = json!(call);
                self.members
                    .observations
                    .get_mut(&id)
                    .unwrap()
                    .projection
                    .tool_lookup(request, call, None);
            }
            let request = self.client.send(method, params).await?;
            self.members
                .observations
                .get_mut(&id)
                .expect("admitted observation")
                .pending = Some(if method == "session.attach" {
                Pending::Attach(request)
            } else if method == "session.detach" {
                Pending::Detach(request)
            } else if method == "session.tool.get" {
                let (origin, call) = missing.unwrap();
                Pending::Tool(request, origin, call)
            } else {
                Pending::Goal(request)
            });
            if method == "session.goal.get" {
                self.members.observations.get_mut(&id).unwrap().goal_stale = false;
            }
        }
        let asked: Vec<String> = self
            .members
            .observations
            .iter_mut()
            .filter(|(_, observation)| {
                observation.earlier == EarlierRead::Asked && observation.pending.is_none()
            })
            .map(|(id, observation)| {
                observation.earlier = EarlierRead::Reading;
                id.clone()
            })
            .collect();
        self.members.pages.extend(asked);
        if self
            .members
            .observations
            .values()
            .any(|observation| matches!(observation.pending, Some(Pending::History(_))))
        {
            return Ok(());
        }
        while let Some(id) = self.members.pages.pop_front() {
            let Some(observation) = self.members.observations.get(&id) else {
                continue;
            };
            if observation.closing
                || observation.pending.is_some()
                || (observation.state != MemberConversationState::Loading
                    && observation.earlier == EarlierRead::None)
            {
                continue;
            }
            if !self.client.supports(&["session.history.latestTurn"]) {
                self.fail_member(
                    &id,
                    "This daemon cannot read a bounded transcript window".into(),
                );
                continue;
            }
            let params = observation.window.params(&id, "main");
            let method = if self
                .client
                .supports(&["session.history.display", "session.history.detail"])
            {
                "session.history.display"
            } else {
                "session.history"
            };
            let request = self.client.send(method, params).await?;
            self.members
                .observations
                .get_mut(&id)
                .expect("admitted observation")
                .pending = Some(Pending::History(request));
            break;
        }
        Ok(())
    }

    /// Names native Agents on the cards with one shared `agent.list`.
    async fn poll_agents(&mut self) -> crate::Result<()> {
        if self.members.agents.is_some()
            || !self
                .task_history
                .get("main")
                .is_some_and(|branch| branch.done)
        {
            return Ok(());
        }
        let asked = self.projection.unnamed_agents();
        if asked.is_empty() {
            return Ok(());
        }
        let id = self.client.send("agent.list", json!({})).await?;
        self.members.agents = Some((id, self.projection_id, asked));
        Ok(())
    }

    pub(super) async fn member_reply(
        &mut self,
        request_id: u64,
        result: Result<serde_json::Value, client::RpcError>,
    ) {
        if let Some((_, projection_id, asked)) =
            self.members.agents.take_if(|(id, _, _)| *id == request_id)
        {
            if projection_id == self.projection_id {
                let names = result
                    .ok()
                    .and_then(|value| serde_json::from_value::<Vec<AgentSummary>>(value).ok())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|agent| (agent.agent_id, agent.name))
                    .collect();
                self.projection.name_agents(names, asked);
                self.changed();
            }
            return;
        }
        let id = self
            .members
            .request(request_id)
            .expect("matched member request");
        let mut next = None;
        let mut ready = false;
        let observation = self
            .members
            .observations
            .get_mut(&id)
            .expect("matched observation");
        let pending = observation.pending.take().expect("matched member request");
        match pending {
            Pending::Attach(_) => match result {
                Ok(value) => {
                    let attached: Attached = match serde_json::from_value(value) {
                        Ok(attached) => attached,
                        Err(error) => {
                            self.fail_member(&id, error.to_string());
                            return;
                        }
                    };
                    observation.model_through = attached.sequence;
                    observation.model = attached
                        .model
                        .map(|model| format!("{} / {}", model.provider_id, model.model_id));
                    observation.attached = true;
                    if !observation.closing {
                        self.members.pages.push_back(id.clone());
                    }
                }
                Err(error) if !observation.closing => self.fail_member(&id, error.to_string()),
                Err(_) => {}
            },
            Pending::Detach(_) => {
                self.members.observations.remove(&id);
                if let Err(error) = result {
                    self.send(Update::Notice(format!("Member {id} detach: {error}")));
                }
            }
            Pending::History(_)
            | Pending::Turn(_)
            | Pending::Methods(_)
            | Pending::Goal(_)
            | Pending::Tool(..)
                if observation.closing => {}
            Pending::Turn(_) => {
                let read = result.map_err(|error| error.to_string()).and_then(|value| {
                    serde_json::from_value::<crate::wire::TurnRead>(value)
                        .map_err(|error| error.to_string())
                });
                match read {
                    Ok(turn) if turn.branch_id == "main" && turn.through == observation.cursor => {
                        observation
                            .projection
                            .set_running(turn.turn.is_some_and(|turn| !turn.ended));
                        if let Some(output) = turn.output {
                            observation.projection.active_output(&output);
                        }
                        next = Some("session.methods");
                    }
                    Ok(_) => self.fail_member(&id, "Member state cutoff changed".into()),
                    Err(error) => self.fail_member(&id, error),
                }
            }
            Pending::Methods(_) => {
                let read = result.map_err(|error| error.to_string()).and_then(|value| {
                    serde_json::from_value::<dim_client::wire::MethodTable>(value)
                        .map_err(|error| error.to_string())
                });
                match read {
                    Ok(table) => {
                        observation.tool_available = table
                            .methods
                            .iter()
                            .any(|method| method.name == "session.tool.get");
                        observation.goal_available = table
                            .methods
                            .iter()
                            .any(|method| method.name == "session.goal.get");
                        observation.goal_stale = false;
                        if observation.goal_available {
                            next = Some("session.goal.get");
                        } else {
                            ready = true;
                        }
                    }
                    Err(error) => self.fail_member(&id, error),
                }
            }
            Pending::Goal(_) => {
                let read = result.map_err(|error| error.to_string()).and_then(|value| {
                    serde_json::from_value::<dim_agent_types::GoalRead>(value)
                        .map_err(|error| error.to_string())
                });
                match read {
                    Ok(read) => {
                        observation.projection.goal = read.goal;
                        ready = true;
                    }
                    Err(error) if observation.state == MemberConversationState::Live => {
                        self.send(Update::Notice(format!("Member {id} goal: {error}")))
                    }
                    Err(error) => self.fail_member(&id, error),
                }
            }
            Pending::Tool(_, request, call) => {
                let read = result.map_err(|error| error.to_string()).and_then(|value| {
                    serde_json::from_value::<dim_agent_types::ToolRead>(value)
                        .map_err(|error| error.to_string())
                });
                match read {
                    Ok(read)
                        if read.branch_id == "main"
                            && read.call.as_ref().is_none_or(|origin| {
                                origin.request_id == request && origin.call.id == call
                            }) =>
                    {
                        observation
                            .projection
                            .tool_lookup(&request, &call, read.call);
                        self.member_changed(&id);
                    }
                    Ok(_) => {
                        self.send(Update::Notice(format!("Member {id} tool identity changed")))
                    }
                    Err(error) => self.send(Update::Notice(format!("Member {id} tool: {error}"))),
                }
            }
            Pending::History(_) => {
                let page = result.map_err(|error| error.to_string()).and_then(|value| {
                    let page = if self
                        .client
                        .supports(&["session.history.display", "session.history.detail"])
                    {
                        serde_json::from_value::<dim_client::wire::DisplayPage>(value)
                            .map(Into::into)
                    } else {
                        serde_json::from_value(value)
                    }
                    .map_err(|error| format!("Invalid history page: {error}"))?;
                    observation
                        .window
                        .push(page)
                        .map_err(|error| error.to_string())
                });
                match page {
                    Err(error) => self.fail_member(&id, error),
                    Ok(done) => {
                        if done {
                            if observation.earlier == EarlierRead::Reading {
                                let mut older = Projection::default();
                                for event in observation.window.take() {
                                    older.replay("main", &event);
                                }
                                observation.projection.prepend(older);
                                observation.earlier = EarlierRead::None;
                            } else {
                                for event in observation.window.take() {
                                    observation.event(event, true);
                                }
                                observation.cursor =
                                    observation.window.through.expect("window cutoff");
                                observation.projection.trim(300);
                                next = Some("session.turn.get");
                            }
                        } else {
                            self.members.pages.push_back(id.clone());
                        }
                        self.member_changed(&id);
                    }
                }
            }
        }
        if let Some(method) = next {
            let mut params = json!({"sessionId":id});
            if method != "session.methods" {
                params["branchId"] = json!("main");
            }
            if method == "session.turn.get" {
                params["through"] = json!(self.members.observations[&id].cursor);
            }
            match self.client.send(method, params).await {
                Ok(request) => {
                    self.members.observations.get_mut(&id).unwrap().pending = Some(match method {
                        "session.turn.get" => Pending::Turn(request),
                        "session.methods" => Pending::Methods(request),
                        "session.goal.get" => Pending::Goal(request),
                        _ => unreachable!(),
                    })
                }
                Err(error) => self.fail_member(&id, error.to_string()),
            }
        }
        if ready {
            let observation = self.members.observations.get_mut(&id).unwrap();
            observation.state = MemberConversationState::Live;
            for notice in std::mem::take(&mut observation.live) {
                observation.apply(notice);
            }
            self.member_changed(&id);
        }
    }

    pub(super) fn earlier_member(&mut self) -> bool {
        let Some((_, id)) = &self.members.selected else {
            return false;
        };
        let Some(observation) = self.members.observations.get_mut(id) else {
            return true;
        };
        if observation.earlier != EarlierRead::None
            || observation.state != MemberConversationState::Live
        {
            return true;
        }
        if observation.window.before.is_none() {
            self.send(Update::Notice("Beginning of conversation".into()));
            return true;
        }
        observation.earlier = if observation.pending.is_some() {
            EarlierRead::Asked
        } else {
            EarlierRead::Reading
        };
        if observation.earlier == EarlierRead::Reading {
            self.members.pages.push_back(id.clone());
        }
        true
    }

    pub(super) fn member_notice(&mut self, session_id: &str, notice: Notice) {
        let observation = self
            .members
            .observations
            .get_mut(session_id)
            .expect("routed observation");
        if observation.state == MemberConversationState::Loading {
            observation.live.push(notice);
            return;
        }
        observation.apply(notice);
        self.member_changed(session_id);
    }

    pub(super) fn member_closed(&mut self, session_id: &str) {
        self.members
            .observations
            .get_mut(session_id)
            .expect("closed observation")
            .state = MemberConversationState::Closed;
        self.end_member(session_id);
    }

    fn fail_member(&mut self, session_id: &str, error: String) {
        let observation = self
            .members
            .observations
            .get_mut(session_id)
            .expect("failed observation");
        observation.state = MemberConversationState::ReadError(error);
        observation.live.clear();
        self.end_member(session_id);
    }

    /// Shows the final state, then releases the observation without
    /// reattaching it for an unfinished card.
    fn end_member(&mut self, session_id: &str) {
        self.member_changed(session_id);
        self.publish_member();
        self.members
            .observations
            .get_mut(session_id)
            .expect("ended observation")
            .release();
        self.members.unavailable.insert(session_id.to_owned());
    }

    /// Cards show what the member now does; the detail follows on publish.
    fn member_changed(&mut self, session_id: &str) {
        let observation = &self.members.observations[session_id];
        if observation.projection_id != self.projection_id {
            return;
        }
        self.projection.observe_member(
            session_id,
            observation.model.as_deref(),
            observation.projection.activity().as_deref(),
            &observation.state,
        );
        self.changed();
    }

    pub(super) fn publish_member(&self) {
        let Some((projection_id, session_id)) = &self.members.selected else {
            return;
        };
        if let Some(observation) = self.members.observations.get(session_id)
            && !observation.closing
            && observation.projection_id == *projection_id
            && *projection_id == self.projection_id
        {
            self.send(Update::MemberConversation {
                projection_id: *projection_id,
                session_id: session_id.clone(),
                snapshot: Box::new(observation.projection.snapshot()),
                state: observation.state.clone(),
                model: observation.model.clone(),
            });
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::Message;

    /// A window whose `before` names the older history left to read.
    fn window(before: Option<u64>, through: u64) -> dim_client::window::Window {
        let mut window = dim_client::window::Window::default();
        window.before = before;
        window.through = Some(through);
        window
    }

    /// A live member observation holding a loaded window.
    fn observation(before: Option<u64>, pending: Option<Pending>) -> Observation {
        Observation {
            projection_id: 0,
            projection: Projection::default(),
            state: MemberConversationState::Live,
            model: None,
            attached: true,
            closing: false,
            pending,
            window: window(before, 9),
            model_through: 0,
            earlier: EarlierRead::None,
            goal_available: false,
            tool_available: false,
            goal_stale: false,
            cursor: 0,
            live: Vec::new(),
        }
    }

    /// Selects `observation` as the detail's member on a fresh worker.
    fn select(worker: &mut Worker, observation: Observation) {
        worker.session_id = "s1".into();
        worker.members.selected = Some((worker.projection_id, "member".into()));
        worker
            .members
            .observations
            .insert("member".into(), observation);
    }

    /// A complete history is at its start whatever else the member runs.
    /// `pending` covers every request the observation makes, and none of them
    /// is an earlier read.
    #[tokio::test]
    async fn a_complete_member_history_is_at_start_while_another_request_is_in_flight() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        select(&mut worker, observation(None, Some(Pending::Turn(1))));
        assert_eq!(
            worker.earlier_view(),
            Earlier::AtStart,
            "a complete history has nothing earlier to read, whatever else is in flight"
        );
    }

    /// An earlier read asked for while another member request is in flight is
    /// recorded, reports `Reading` from then, and goes out once that request
    /// settles, so a press is never dropped.
    #[tokio::test]
    async fn a_page_up_during_another_member_request_reads_once_it_settles() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        select(&mut worker, observation(Some(7), Some(Pending::Goal(1))));
        worker.earlier_member();
        assert_eq!(
            worker.earlier_view(),
            Earlier::Reading,
            "the recorded earlier read is reported from the moment it is recorded"
        );
        worker
            .message(Message::Response {
                id: 1,
                result: Ok(json!({"branchId":"member","through":9,"goal":null})),
            })
            .await
            .unwrap();
        assert!(
            worker.members.observations["member"].pending.is_none(),
            "the goal read settled"
        );
        worker.poll_member().await.unwrap();
        assert!(
            matches!(
                worker.members.observations["member"].pending,
                Some(Pending::History(_))
            ),
            "the recorded earlier read went out once nothing else was in flight"
        );
    }

    /// A failed earlier read releases the observation, and a released member
    /// has nothing to read: the detail shows the error and the member is
    /// reattached to be read again.
    #[tokio::test]
    async fn a_failed_member_read_releases_the_observation_and_its_flag() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        let mut observation = observation(Some(7), None);
        observation.earlier = EarlierRead::Reading;
        select(&mut worker, observation);
        assert_eq!(
            worker.earlier_view(),
            Earlier::Reading,
            "the earlier read is in flight"
        );

        worker.fail_member("member", "the daemon dropped the read".into());
        assert!(worker.members.observations["member"].closing);
        assert_eq!(
            worker.earlier_view(),
            Earlier::AtStart,
            "a released observation reports its start, not a read in flight"
        );
    }

    /// A projection change releases the observation the old attachment owned,
    /// and a released observation has nothing left to read.
    #[tokio::test]
    async fn a_member_released_by_a_projection_change_reports_its_start() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        select(&mut worker, observation(Some(7), None));
        worker.earlier_member();
        assert_eq!(
            worker.earlier_view(),
            Earlier::Reading,
            "the earlier read went out"
        );

        worker.projection_id += 1;
        worker.poll_member().await.unwrap();

        assert!(
            worker.members.observations["member"].closing,
            "the observation the previous attachment owned is released"
        );
        assert_eq!(
            worker.earlier_view(),
            Earlier::AtStart,
            "a released observation reports its start, not the read it had in flight"
        );
    }

    /// A closed member reports its start: its window is gone with the Session.
    #[tokio::test]
    async fn a_closed_member_reports_its_start() {
        let (mut worker, _receiver, _sent) =
            crate::session_host::test_worker(&crate::session_host::TEST_FEATURES).await;
        select(&mut worker, observation(Some(7), None));
        assert_eq!(
            worker.earlier_view(),
            Earlier::Unread,
            "a live member with older history is unread"
        );

        worker.member_closed("member");
        assert_eq!(
            worker.earlier_view(),
            Earlier::AtStart,
            "a closed member has no earlier history to read"
        );
    }
}

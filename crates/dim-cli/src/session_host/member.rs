//! Read-only member observations on the leader's existing connection: one
//! per member Session that an unfinished delegation card or the selected
//! detail needs, shared by both. History pages go through one slot.

use std::collections::{HashMap, HashSet, VecDeque};

use super::*;
use crate::model::view::MemberConversationState;

pub(super) enum Notice {
    Event(Event),
    Progress(Progress),
    State(bool),
}

enum Pending {
    Attach(u64),
    History(u64),
    Detach(u64),
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
    after: u64,
    through: Option<u64>,
    cursor: u64,
    live: Vec<Notice>,
}

impl Observation {
    fn apply(&mut self, notice: Notice) {
        match notice {
            Notice::Event(event) if event.sequence > self.cursor => {
                self.cursor = event.sequence;
                if let EventContent::Configuration(configuration) = &event.content {
                    self.model = Some(format!(
                        "{} / {}",
                        configuration.provider_id, configuration.model_id
                    ));
                }
                self.projection.apply("main", &event);
            }
            Notice::Event(_) => {}
            Notice::Progress(progress) => self.projection.progress(&progress),
            Notice::State(running) => self.projection.set_running(running),
        }
    }
}

#[derive(Default)]
pub(super) struct Members {
    observations: HashMap<String, Observation>,
    /// The detail's member: the attachment it was opened on and its Session.
    selected: Option<(u64, String)>,
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
                Some(Pending::Attach(at) | Pending::History(at) | Pending::Detach(at)) if at == request_id
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
                observation.closing = true;
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
                        after: 0,
                        through: None,
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
            let method = if observation.closing && !observation.attached {
                self.members.observations.remove(&id);
                continue;
            } else if observation.closing {
                "session.detach"
            } else if !observation.attached {
                "session.attach"
            } else {
                continue;
            };
            let request = self.client.send(method, json!({"sessionId":id})).await?;
            self.members
                .observations
                .get_mut(&id)
                .expect("admitted observation")
                .pending = Some(if method == "session.attach" {
                Pending::Attach(request)
            } else {
                Pending::Detach(request)
            });
        }
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
                || observation.state != MemberConversationState::Loading
            {
                continue;
            }
            let mut params = json!({
                "sessionId":id,"branchId":"main",
                "after":observation.after,"limit":HISTORY_PAGE,
            });
            if let Some(through) = observation.through {
                params["through"] = json!(through);
            }
            let request = self.client.send("session.history", params).await?;
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

    pub(super) fn member_reply(
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
        let observation = self
            .members
            .observations
            .get_mut(&id)
            .expect("matched observation");
        let pending = observation.pending.take().expect("matched member request");
        match pending {
            Pending::Attach(_) => match result {
                Ok(_) => {
                    observation.attached = true;
                    if !observation.closing {
                        self.members.pages.push_back(id);
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
            Pending::History(_) if observation.closing => {}
            Pending::History(_) => {
                let page = result.map_err(|error| error.to_string()).and_then(|value| {
                    History::decode(value, observation.after, observation.through)
                });
                match page {
                    Err(error) => self.fail_member(&id, error),
                    Ok(page) => {
                        observation.after = page.next_after;
                        observation.through = Some(page.through);
                        for event in page.events {
                            observation.apply(Notice::Event(event));
                        }
                        observation.cursor = page.next_after;
                        if page.done {
                            observation.state = MemberConversationState::Live;
                            for notice in std::mem::take(&mut observation.live) {
                                observation.apply(notice);
                            }
                        } else {
                            self.members.pages.push_back(id.clone());
                        }
                        self.member_changed(&id);
                    }
                }
            }
        }
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
            .closing = true;
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

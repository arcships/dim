use dim_agent_types::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Scope origins survive revocation; only the fixed branch ancestry establishes visibility.
pub(crate) fn scope_is_visible(scope: &PromptScope, branch: &str, ancestors: &[String]) -> bool {
    let (PromptScope::Branch { timeline }
    | PromptScope::Turn { timeline, .. }
    | PromptScope::Task { timeline, .. }) = scope;
    timeline == branch || ancestors.contains(timeline)
}

/// Rebuildable prompt state. Loading and publication belong to capabilities.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromptSnapshot {
    pub configuration: Option<ModelConfiguration>,
    pub entries: BTreeMap<(String, PromptScope), PromptEntry>,
    pub revision: Option<PromptReference>,
    pub system_revision: Option<PromptReference>,
    pub system: String,
    pub(crate) turns: BTreeSet<String>,
    pub(crate) tasks: BTreeSet<PromptScope>,
}

impl PromptSnapshot {
    pub fn from_checkpoint(state: &PromptCheckpointState) -> Self {
        Self {
            configuration: None,
            entries: state
                .entries
                .iter()
                .map(|entry| ((entry.source.clone(), entry.scope.clone()), entry.clone()))
                .collect(),
            revision: state.revision.clone(),
            system_revision: state.system_revision.clone(),
            system: state.system.clone(),
            turns: state.turns.iter().cloned().collect(),
            tasks: state.tasks.iter().cloned().collect(),
        }
    }

    pub fn checkpoint(&self) -> PromptCheckpointState {
        PromptCheckpointState {
            entries: self.entries.values().cloned().collect(),
            revision: self.revision.clone(),
            system_revision: self.system_revision.clone(),
            system: self.system.clone(),
            turns: self.turns.iter().cloned().collect(),
            tasks: self.tasks.iter().cloned().collect(),
        }
    }

    pub fn from_context(context: &dim_agent_runtime::ContextHistory) -> Self {
        let mut state = context
            .checkpoint
            .as_ref()
            .map(|checkpoint| Self::from_checkpoint(&checkpoint.state.prompts))
            .unwrap_or_default();
        state.configuration = context
            .checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.state.configuration.as_ref())
            .map(|data| (*data.settings).clone());
        for event in &context.events {
            state.apply(event);
        }
        state
    }

    pub fn from_events(events: &[Arc<AgentEvent>]) -> Self {
        let mut state = Self::default();
        for event in events {
            state.apply(event);
        }
        state
    }

    pub fn active(&self, scope: &PromptScope) -> bool {
        match scope {
            PromptScope::Branch { .. } => true,
            PromptScope::Turn { id, .. } => self.turns.contains(id),
            PromptScope::Task { .. } => self.tasks.contains(scope),
        }
    }

    pub(crate) fn changed(&mut self, timeline: &str, revision: &str, changes: &[PromptChange]) {
        let revision = PromptReference {
            timeline: timeline.to_owned(),
            revision: revision.to_owned(),
        };
        let mut system_changed = false;
        for change in changes {
            let (source, scope) = match change {
                PromptChange::Set { entry } => (&entry.source, &entry.scope),
                PromptChange::Revoke { source, scope } => (source, scope),
            };
            let key = (source.clone(), scope.clone());
            system_changed |= self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.delivery == PromptDelivery::System);
            match change {
                PromptChange::Set { entry } if self.active(scope) => {
                    system_changed |= entry.delivery == PromptDelivery::System;
                    self.entries.insert(key, entry.clone());
                }
                PromptChange::Set { .. } => {}
                PromptChange::Revoke { .. } => {
                    self.entries.remove(&key);
                }
            }
        }
        self.revision = Some(revision.clone());
        if system_changed {
            self.system_revision = Some(revision);
            self.rebuild_system();
        }
    }

    pub(crate) fn system_after(&self, changes: &[PromptChange]) -> Option<String> {
        let changed = changes.iter().any(|change| {
            let (source, scope, system) = match change {
                PromptChange::Set { entry } => (
                    &entry.source,
                    &entry.scope,
                    entry.delivery == PromptDelivery::System,
                ),
                PromptChange::Revoke { source, scope } => (source, scope, false),
            };
            system
                || self
                    .entries
                    .get(&(source.clone(), scope.clone()))
                    .is_some_and(|entry| entry.delivery == PromptDelivery::System)
        });
        if !changed {
            return None;
        }
        let mut entries: BTreeMap<_, _> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.delivery == PromptDelivery::System)
            .map(|(key, entry)| ((&key.0, &key.1), entry))
            .collect();
        for change in changes {
            match change {
                PromptChange::Set { entry } if entry.delivery == PromptDelivery::System => {
                    entries.insert((&entry.source, &entry.scope), entry);
                }
                PromptChange::Set { entry } => {
                    entries.remove(&(&entry.source, &entry.scope));
                }
                PromptChange::Revoke { source, scope } => {
                    entries.remove(&(source, scope));
                }
            }
        }
        Some(system_text(entries.into_values().collect()))
    }

    pub(crate) fn rebuild_system(&mut self) {
        self.system = system_text(
            self.entries
                .values()
                .filter(|entry| entry.delivery == PromptDelivery::System)
                .collect(),
        );
    }

    pub fn apply(&mut self, event: &AgentEvent) {
        self.apply_kind(&event.kind);
    }

    pub(crate) fn apply_kind(&mut self, kind: &EventKind) {
        match kind {
            EventKind::ContextConfigured(data) => {
                self.configuration = Some((*data.settings).clone());
                return;
            }
            EventKind::PromptChanged(data) => {
                self.changed(&data.timeline, &data.revision, &data.changes);
                return;
            }
            EventKind::TurnStart(data) => {
                self.set_turn_active(&data.turn_id, true);
                return;
            }
            EventKind::TurnEnd(data) => {
                self.set_turn_active(&data.turn_id, false);
                return;
            }
            EventKind::BranchStart(_) => self.turns.clear(),
            EventKind::ContextScope(data) => match data.status {
                ContextScopeStatus::Started => {
                    self.tasks.insert(data.scope.clone());
                }
                ContextScopeStatus::Ended => {
                    self.tasks.remove(&data.scope);
                }
            },
            _ => return,
        }
        self.retain_active();
    }

    pub(crate) fn set_turn_active(&mut self, turn_id: &str, active: bool) {
        if active {
            self.turns.insert(turn_id.to_owned());
        } else {
            self.turns.remove(turn_id);
        }
        self.retain_active();
    }

    fn retain_active(&mut self) {
        let turns = &self.turns;
        let tasks = &self.tasks;
        self.entries.retain(|(_, scope), _| match scope {
            PromptScope::Branch { .. } => true,
            PromptScope::Turn { id, .. } => turns.contains(id),
            PromptScope::Task { .. } => tasks.contains(scope),
        });
    }
}

fn system_text(mut entries: Vec<&PromptEntry>) -> String {
    entries.sort_by(|a, b| (a.order, &a.source, &a.scope).cmp(&(b.order, &b.source, &b.scope)));
    entries
        .into_iter()
        .map(|entry| entry.body.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

use dim_agent_runtime::{
    AgentError, QueryFilter, QueryOperand, QueryOutput, QueryPage, QueryPick, QuerySelect,
    QuerySpec, QueryValue, Timeline, TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::*;
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
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
    turns: BTreeSet<String>,
    tasks: BTreeSet<PromptScope>,
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
            let mut entries: Vec<_> = self
                .entries
                .values()
                .filter(|entry| entry.delivery == PromptDelivery::System)
                .collect();
            entries.sort_by(|a, b| {
                a.order
                    .cmp(&b.order)
                    .then(a.source.cmp(&b.source))
                    .then(a.scope.cmp(&b.scope))
            });
            self.system = entries
                .into_iter()
                .map(|entry| entry.body.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
        }
    }

    pub(crate) async fn read(
        timeline: &Timeline,
        through: u64,
    ) -> dim_agent_runtime::Result<(Self, Option<ContextConfiguredData>)> {
        let mut query = TimelineQuery {
            scope: TimelineQueryScope::Chain,
            from: None,
            through: Some(through),
            include_publishing: false,
            query: QuerySpec {
                filter: Some(QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: vec!["context/configured".into()],
                }),
                select: QuerySelect::Fields {
                    fields: vec!["data".into()],
                },
                group_by: vec![],
                pick: Some(QueryPick::Last),
                order_by: vec![],
            },
            page: Some(QueryPage {
                limit: std::num::NonZeroU64::MIN,
                after: None,
            }),
        };
        let reply = timeline.query(query.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("configuration fields")
        };
        let configuration: Option<ContextConfiguredData> = rows.into_iter().next().map(|row| {
            let [QueryValue::Value { value }] =
                <[_; 1]>::try_from(row.values).expect("one configuration field")
            else {
                unreachable!("configuration has data")
            };
            serde_json::from_value(value).expect("configuration follows its event contract")
        });
        query.query.filter = Some(QueryFilter::In {
            value: QueryOperand::Field {
                path: "kind".into(),
            },
            values: [
                "prompt/changed",
                "turn/start",
                "turn/end",
                "branch/start",
                "context/scope",
            ]
            .map(Into::into)
            .to_vec(),
        });
        query.query.select = QuerySelect::Fields {
            fields: [
                "kind",
                "data.timeline",
                "data.revision",
                "data.changes",
                "data.inputs",
                "data.turn_id",
                "data.scope",
                "data.status",
                "position",
            ]
            .map(String::from)
            .to_vec(),
        };
        query.query.pick = None;
        query.page.as_mut().unwrap().limit = std::num::NonZeroU64::new(256).unwrap();
        // Chain order names the branches a fact can lie in, root first. The
        // first fold finds the ancestor changes that name delivery inputs;
        // without any, every fact applies where it is found and that fold is
        // the answer. Otherwise the second fold applies those changes where
        // their inputs were consumed, or drops them undelivered.
        let chain: Vec<String> = timeline
            .ancestors()
            .into_iter()
            .map(|(branch, _)| branch)
            .chain(std::iter::once(timeline.branch_id()))
            .collect();
        let (mut snapshot, named_inputs) =
            Self::folded(timeline, query.clone(), &chain, None).await?;
        if !named_inputs.is_empty() {
            let deliveries = consumed_inputs(timeline, through, &named_inputs).await?;
            snapshot = Self::folded(timeline, query, &chain, Some(&deliveries))
                .await?
                .0;
        }
        snapshot.configuration = configuration.as_ref().map(|data| (*data.settings).clone());
        Ok((snapshot, configuration))
    }

    /// Folds the chain's prompt facts at the query's `through`. A change on a
    /// line other than `timeline`'s own applies only once every input it
    /// names was consumed in the inherited prefix: `deliveries` names each
    /// such input's consumption position, and the change applies where the
    /// fold reaches the last of them. A change with an input never consumed
    /// there is dropped; one with only some consumed counts as undelivered.
    /// A change's System entries are not delivered by an input block (the
    /// prefix's configuration already carries them), so they apply where the
    /// fact is found even when the rest of the change waits. Without
    /// `deliveries` every fact applies where it is found; the returned set
    /// names the inputs the fold would have waited for.
    async fn folded(
        timeline: &Timeline,
        mut query: TimelineQuery,
        chain: &[String],
        deliveries: Option<&BTreeMap<String, TimelinePosition>>,
    ) -> dim_agent_runtime::Result<(Self, BTreeSet<String>)> {
        /// A change waiting for delivery: where its last input was consumed,
        /// where the change itself was written (changes delivered at one
        /// position apply in their sequence order), and its `deferred` index.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        struct Waiting {
            delivery: (usize, u64),
            written: (usize, u64),
            change: usize,
        }
        let branch = timeline.branch_id();
        let at = |position: &TimelinePosition| -> dim_agent_runtime::Result<(usize, u64)> {
            let index = chain
                .iter()
                .position(|branch| *branch == position.branch_id)
                .ok_or_else(|| {
                    AgentError::Storage(format!(
                        "a prompt fact names branch {} outside the queried chain",
                        position.branch_id
                    ))
                })?;
            Ok((index, position.sequence))
        };
        let mut snapshot = Self::default();
        let mut needed = BTreeSet::new();
        let mut deferred: Vec<(String, String, Vec<PromptChange>)> = Vec::new();
        let mut pending: BinaryHeap<Reverse<Waiting>> = BinaryHeap::new();
        loop {
            let reply = timeline.query(query.clone()).await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("prompt state fields")
            };
            for row in rows {
                let [
                    QueryValue::Value {
                        value: serde_json::Value::String(kind),
                    },
                    line,
                    revision,
                    changes,
                    inputs,
                    turn,
                    scope,
                    status,
                    position,
                ] = <[_; 9]>::try_from(row.values).expect("prompt state fields")
                else {
                    unreachable!("a prompt fact has a kind")
                };
                let QueryValue::Value { value: position } = position else {
                    unreachable!("a prompt fact has a position")
                };
                let position: TimelinePosition = serde_json::from_value(position)
                    .expect("a position follows the query contract");
                let here = at(&position)?;
                while let Some(Reverse(waiting)) = pending.peek().copied() {
                    if waiting.delivery >= here {
                        break;
                    }
                    let Reverse(waiting) = pending.pop().expect("a pending change");
                    let (line, revision, changes) = &deferred[waiting.change];
                    snapshot.changed(line, revision, changes);
                }
                match kind.as_str() {
                    "prompt/changed" => {
                        let (
                            QueryValue::Value {
                                value: serde_json::Value::String(line),
                            },
                            QueryValue::Value {
                                value: serde_json::Value::String(revision),
                            },
                            QueryValue::Value { value: changes },
                            inputs,
                        ) = (line, revision, changes, inputs)
                        else {
                            unreachable!("a prompt change has identity and changes")
                        };
                        let changes: Vec<PromptChange> = serde_json::from_value(changes)
                            .expect("prompt changes follow their event contract");
                        let inputs: Vec<String> = match inputs {
                            QueryValue::Value { value } => serde_json::from_value(value)
                                .expect("prompt change inputs follow their event contract"),
                            QueryValue::Missing => Vec::new(),
                            _ => unreachable!("a prompt change's inputs are a list"),
                        };
                        let inherited = line != branch && !inputs.is_empty();
                        if inherited {
                            needed.extend(inputs.iter().cloned());
                        }
                        match (inherited, deliveries) {
                            (true, Some(deliveries)) => {
                                let mut waiting = Vec::new();
                                for change in changes {
                                    if snapshot.system_delivered(&change) {
                                        snapshot.changed(
                                            &line,
                                            &revision,
                                            std::slice::from_ref(&change),
                                        );
                                    } else {
                                        // A source that leaves System for an
                                        // input-delivered class: the prefix's
                                        // configuration already carries the
                                        // new System composition without it, so
                                        // only the new value waits.
                                        if let Some(removal) = snapshot.system_removal(&change) {
                                            snapshot.changed(
                                                &line,
                                                &revision,
                                                std::slice::from_ref(&removal),
                                            );
                                        }
                                        waiting.push(change);
                                    }
                                }
                                let mut delivery: Option<(usize, u64)> = None;
                                for input in &inputs {
                                    let Some(position) = deliveries.get(input) else {
                                        delivery = None;
                                        break;
                                    };
                                    let position = at(position)?;
                                    delivery =
                                        Some(delivery.map_or(position, |last| last.max(position)));
                                }
                                if let Some(delivery) = delivery
                                    && !waiting.is_empty()
                                {
                                    let change = deferred.len();
                                    deferred.push((line, revision, waiting));
                                    pending.push(Reverse(Waiting {
                                        delivery,
                                        written: here,
                                        change,
                                    }));
                                }
                            }
                            _ => snapshot.changed(&line, &revision, &changes),
                        }
                    }
                    "turn/start" | "turn/end" => {
                        let QueryValue::Value {
                            value: serde_json::Value::String(turn),
                        } = turn
                        else {
                            unreachable!("a turn has an identity")
                        };
                        if kind == "turn/start" {
                            snapshot.turns.insert(turn);
                        } else {
                            snapshot.turns.remove(&turn);
                        }
                    }
                    "branch/start" => snapshot.turns.clear(),
                    "context/scope" => {
                        let (
                            QueryValue::Value { value: scope },
                            QueryValue::Value { value: status },
                        ) = (scope, status)
                        else {
                            unreachable!("scope event fields")
                        };
                        let scope: PromptScope = serde_json::from_value(scope)
                            .expect("prompt scope follows its event contract");
                        match serde_json::from_value::<ContextScopeStatus>(status)
                            .expect("scope status follows its event contract")
                        {
                            ContextScopeStatus::Started => {
                                snapshot.tasks.insert(scope);
                            }
                            ContextScopeStatus::Ended => {
                                snapshot.tasks.remove(&scope);
                            }
                        }
                    }
                    _ => unreachable!("selected prompt state kind"),
                }
                snapshot.retain_active();
            }
            query.page.as_mut().unwrap().after = reply.next;
            if query.page.as_ref().unwrap().after.is_none() {
                break;
            }
        }
        while let Some(Reverse(waiting)) = pending.pop() {
            let (line, revision, changes) = &deferred[waiting.change];
            snapshot.changed(line, revision, changes);
        }
        snapshot.retain_active();
        Ok((snapshot, needed))
    }

    /// Whether the change takes effect through the System prompt rather than
    /// an input block: a System entry is already in the prefix's
    /// configuration, so another line's copy of it is delivered.
    fn system_delivered(&self, change: &PromptChange) -> bool {
        match change {
            PromptChange::Set { entry } => entry.delivery == PromptDelivery::System,
            PromptChange::Revoke { source, scope } => self
                .entries
                .get(&(source.clone(), scope.clone()))
                .is_some_and(|entry| entry.delivery == PromptDelivery::System),
        }
    }

    /// The removal of the System entry `change` replaces, when the entry in
    /// force for its source is System and the change is not: the source leaves
    /// the System prompt, and the prefix's configuration already carries that
    /// composition, so the removal applies where the change is found while
    /// only the new value waits for its delivery.
    fn system_removal(&self, change: &PromptChange) -> Option<PromptChange> {
        let PromptChange::Set { entry } = change else {
            return None;
        };
        if entry.delivery == PromptDelivery::System {
            return None;
        }
        let key = (entry.source.clone(), entry.scope.clone());
        let replaced = self.entries.get(&key)?;
        (replaced.delivery == PromptDelivery::System).then(|| PromptChange::Revoke {
            source: entry.source.clone(),
            scope: entry.scope.clone(),
        })
    }

    pub fn apply(&mut self, event: &AgentEvent) {
        match &event.kind {
            EventKind::ContextConfigured(data) => {
                self.configuration = Some((*data.settings).clone());
            }
            EventKind::PromptChanged(data) => {
                self.changed(&data.timeline, &data.revision, &data.changes)
            }
            EventKind::TurnStart(data) => {
                self.turns.insert(data.turn_id.clone());
            }
            EventKind::TurnEnd(data) => {
                self.turns.remove(&data.turn_id);
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

/// The chain's consumption of each of `inputs`, by position. A consumption
/// after an ancestor's cut is outside the chain and does not count: the line
/// in hand inherited that change undelivered.
async fn consumed_inputs(
    timeline: &Timeline,
    through: u64,
    inputs: &BTreeSet<String>,
) -> dim_agent_runtime::Result<BTreeMap<String, TimelinePosition>> {
    let mut query = TimelineQuery {
        scope: TimelineQueryScope::Chain,
        from: None,
        through: Some(through),
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::And {
                conditions: vec![
                    QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: vec!["input/consumed".into()],
                    },
                    QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "data.input_id".into(),
                        },
                        values: inputs.iter().cloned().map(Into::into).collect(),
                    },
                ],
            }),
            select: QuerySelect::Fields {
                fields: ["data.input_id", "position"].map(String::from).to_vec(),
            },
            group_by: vec![],
            pick: None,
            order_by: vec![],
        },
        page: Some(QueryPage {
            limit: std::num::NonZeroU64::new(256).unwrap(),
            after: None,
        }),
    };
    let mut consumed = BTreeMap::new();
    loop {
        let reply = timeline.query(query.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("consumed inputs")
        };
        for row in rows {
            let [
                QueryValue::Value {
                    value: serde_json::Value::String(input),
                },
                QueryValue::Value { value: position },
            ] = <[_; 2]>::try_from(row.values).expect("a consumption has an input")
            else {
                unreachable!("a consumption names its input")
            };
            consumed.insert(
                input,
                serde_json::from_value(position).expect("a position follows the query contract"),
            );
        }
        query.page.as_mut().unwrap().after = reply.next;
        if query.page.as_ref().unwrap().after.is_none() {
            return Ok(consumed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dim_agent_runtime::{MemoryTimelineStore, SessionConfig, TimelineKey, TimelineStore};
    use rutis::host::CtxHost;

    /// One line over a fresh store; `ancestors` names the inherited prefix.
    async fn line(
        branch: &str,
        ancestors: Vec<(String, u64)>,
    ) -> (Timeline, Arc<MemoryTimelineStore>) {
        let store = Arc::new(MemoryTimelineStore::default());
        store
            .create_session("s", &SessionConfig::default())
            .await
            .unwrap();
        let timeline = Timeline::new(
            rutis::Ctx::root().unwrap(),
            TimelineKey {
                session_id: "s".into(),
                branch_id: branch.into(),
            },
            ancestors,
            store.clone(),
        );
        (timeline, store)
    }

    /// A child of `main` cut at `cut`; the branch is registered by the
    /// `branch/create` the parent wrote after the cut.
    fn branch(main: &Timeline, store: Arc<MemoryTimelineStore>, cut: u64) -> Timeline {
        main.append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: BranchSource::Host {
                    operation_id: "test".into(),
                },
                branch: "child".into(),
                prompt: "child".into(),
            })
            .into(),
        )
        .unwrap();
        Timeline::new(
            rutis::Ctx::root().unwrap(),
            TimelineKey {
                session_id: "s".into(),
                branch_id: "child".into(),
            },
            vec![("main".into(), cut)],
            store,
        )
    }

    /// A Branch-scope State entry of `main` whose body is `body`.
    fn entry(body: &str) -> PromptEntry {
        PromptEntry {
            source: "dim/test".into(),
            version: "1".into(),
            body: body.into(),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
            delivery: PromptDelivery::State,
            order: 0,
        }
    }

    fn set(body: &str) -> PromptChange {
        PromptChange::Set { entry: entry(body) }
    }

    /// A change `line` wrote, delivering through `inputs`.
    fn changed(line: &str, revision: &str, change: PromptChange, inputs: &[&str]) -> EventDraft {
        EventDraft::new(EventKind::PromptChanged(PromptChangedData {
            timeline: line.into(),
            revision: revision.into(),
            previous: None,
            reason: PromptChangeReason::Refresh,
            changes: vec![change],
            inputs: inputs.iter().map(|input| (*input).to_owned()).collect(),
        }))
    }

    /// An Append input named `id`, the delivery a change names.
    fn input(id: &str) -> EventDraft {
        EventDraft::new(EventKind::InputMessage(InputMessageData {
            blocks: vec![InputBlock::Raw {
                text: format!("input {id}"),
                key: None,
                retire: false,
            }],
            mode: Some(InputMode::Append),
            ..Default::default()
        }))
        .with_id(id)
    }

    fn consumed(id: &str) -> EventDraft {
        EventDraft::new(EventKind::InputConsumed(InputConsumedData {
            input_id: id.into(),
        }))
    }

    async fn read(timeline: &Timeline) -> PromptSnapshot {
        let (snapshot, _) = PromptSnapshot::read(timeline, timeline.committed())
            .await
            .unwrap();
        snapshot
    }

    fn bodies(snapshot: &PromptSnapshot) -> Vec<&str> {
        snapshot
            .entries
            .values()
            .map(|entry| entry.body.as_str())
            .collect()
    }

    /// A change the reading line wrote applies where it is found, even while
    /// its inputs are still unconsumed.
    #[tokio::test]
    async fn an_own_line_change_applies_before_its_inputs_are_consumed() {
        let (main, _) = line("main", vec![]).await;
        main.append(changed("main", "r1", set("own"), &["i1"]))
            .unwrap();
        main.append(input("i1")).unwrap();
        assert_eq!(bodies(&read(&main).await), ["own"]);
    }

    /// An inherited change whose inputs the prefix consumed applies.
    #[tokio::test]
    async fn an_ancestor_change_applies_once_its_inputs_are_consumed() {
        let (main, store) = line("main", vec![]).await;
        main.append(changed("main", "r1", set("delivered"), &["i1"]))
            .unwrap();
        main.append(input("i1")).unwrap();
        main.append(consumed("i1")).unwrap();
        let child = branch(&main, store, main.committed());
        assert_eq!(bodies(&read(&child).await), ["delivered"]);
    }

    /// An inherited change whose input the prefix never consumed is not in
    /// the line's state: the block it would deliver is not in the prefix.
    #[tokio::test]
    async fn an_ancestor_change_with_an_unconsumed_input_is_not_applied() {
        let (main, store) = line("main", vec![]).await;
        main.append(changed("main", "r1", set("pending"), &["i1"]))
            .unwrap();
        main.append(input("i1")).unwrap();
        let child = branch(&main, store, main.committed());
        assert!(bodies(&read(&child).await).is_empty());
    }

    /// Consuming only some of a change's inputs does not deliver it.
    #[tokio::test]
    async fn an_ancestor_change_with_one_input_unconsumed_is_not_applied() {
        let (main, store) = line("main", vec![]).await;
        main.append(changed("main", "r1", set("two"), &["i1", "i2"]))
            .unwrap();
        main.append(input("i1")).unwrap();
        main.append(input("i2")).unwrap();
        main.append(consumed("i1")).unwrap();
        let child = branch(&main, store, main.committed());
        assert!(bodies(&read(&child).await).is_empty());
    }

    /// A change written before the field existed names no inputs and applies
    /// where it is found.
    #[tokio::test]
    async fn an_ancestor_change_without_inputs_applies() {
        let (main, store) = line("main", vec![]).await;
        main.append(changed("main", "r1", set("old fact"), &[]))
            .unwrap();
        let child = branch(&main, store, main.committed());
        assert_eq!(bodies(&read(&child).await), ["old fact"]);
    }

    /// Two inherited changes delivered by one request apply where their
    /// inputs were consumed — the order the blocks rendered — not where the
    /// changes were written.
    #[tokio::test]
    async fn inherited_changes_delivered_by_one_request_apply_in_delivery_order() {
        let (main, store) = line("main", vec![]).await;
        main.append(changed("main", "r1", set("first written"), &["i1"]))
            .unwrap();
        main.append(input("i1")).unwrap();
        main.append(changed("main", "r2", set("second written"), &["i2"]))
            .unwrap();
        main.append(input("i2")).unwrap();
        // The request consumes the later change's input first.
        main.append(consumed("i2")).unwrap();
        main.append(consumed("i1")).unwrap();
        let child = branch(&main, store, main.committed());
        assert_eq!(
            bodies(&read(&child).await),
            ["first written"],
            "the input consumed last renders last"
        );
    }

    /// A change mixing System content with input-delivered entries applies
    /// its System parts where it is found — the prefix's configuration
    /// already carries them — and holds the rest for delivery.
    #[tokio::test]
    async fn a_mixed_inherited_change_applies_its_system_part_before_delivery() {
        let (main, store) = line("main", vec![]).await;
        let system = |source: &str, body: &str| PromptEntry {
            source: source.into(),
            delivery: PromptDelivery::System,
            body: body.into(),
            ..entry(body)
        };
        let first = changed(
            "main",
            "r0",
            PromptChange::Set {
                entry: system("dim/base", "base"),
            },
            &[],
        );
        let mixed = EventDraft::new(EventKind::PromptChanged(PromptChangedData {
            timeline: "main".into(),
            revision: "r1".into(),
            previous: None,
            reason: PromptChangeReason::Refresh,
            changes: vec![
                PromptChange::Set {
                    entry: system("dim/session", "session"),
                },
                set("state"),
            ],
            inputs: vec!["i1".into()],
        }));
        let revoke = changed(
            "main",
            "r2",
            PromptChange::Revoke {
                source: "dim/base".into(),
                scope: PromptScope::Branch {
                    timeline: "main".into(),
                },
            },
            &["i2"],
        );
        main.append(first).unwrap();
        main.append(mixed).unwrap();
        main.append(revoke).unwrap();
        let child = branch(&main, store, main.committed());
        let snapshot = read(&child).await;
        assert_eq!(
            bodies(&snapshot),
            ["session"],
            "only the System part applies"
        );
        assert_eq!(snapshot.system, "session", "the System composition follows");
    }
}

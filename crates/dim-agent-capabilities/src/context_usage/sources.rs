use super::*;
use crate::history::{chain_by_id, render_input, settled_request_output, tool_result_message};
use crate::host::ModelSettings;
use dim_agent_runtime::{QueryOrder, QueryOrderKey, TimelineQueryDirection};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Clone)]
pub(super) struct RequestBase {
    pub epoch: String,
    pub source: TimelinePosition,
    pub algorithm: String,
    pub current: Option<NumericResult>,
    pub legacy_result: Option<String>,
    pub initialized: Key,
}

fn decode<T: DeserializeOwned>(value: QueryValue) -> Result<T> {
    let QueryValue::Value { value } = value else {
        return Err(fault("missing required usage property"));
    };
    serde_json::from_value(value).map_err(|error| fault(error.to_string()))
}

pub(super) fn optional<T: DeserializeOwned>(value: QueryValue) -> Result<Option<T>> {
    match value {
        QueryValue::Missing => Ok(None),
        value => decode(value).map(Some),
    }
}

fn elements(value: QueryValue) -> Result<Vec<QueryValue>> {
    let QueryValue::Elements { values } = value else {
        return Err(fault("missing usage array projection"));
    };
    Ok(values)
}

fn measured_prompt(usage: &Usage) -> Option<f64> {
    (usage.prompt_tokens.is_finite()
        && usage.prompt_tokens > 0.
        && usage.prompt_tokens
            >= usage.cache_read_tokens.unwrap_or(0.) + usage.cache_write_tokens.unwrap_or(0.))
    .then_some(usage.prompt_tokens)
}

fn message(
    summary: &mut Summary,
    counter: &dyn crate::host::ContextCounter,
    empty: (Contribution, f64),
    message: &Message,
) -> Result<()> {
    let contribution = counter.message(message);
    let growth = counter.estimate(add(empty.0, contribution)?) - empty.1;
    if !growth.is_finite() || growth < 0. {
        return Err(fault("invalid message growth contribution"));
    }
    summary.ordinary = add(summary.ordinary, contribution)?;
    summary.message_count = summary
        .message_count
        .map(|count| {
            count
                .checked_add(1)
                .ok_or_else(|| fault("message count overflow"))
        })
        .transpose()?;
    summary.growth = Some(summary.growth.unwrap_or(0.) + growth);
    Ok(())
}

impl ContextUsage {
    pub(super) async fn run(&self) -> Result<()> {
        let initial = self.timeline.sequence();
        let mut machine = self.restore(initial).await?;
        self.association
            .lock()
            .unwrap()
            .measurements
            .retain(|sequence, _| *sequence > initial);
        self.publish(&machine, initial);
        let mut covered = initial;
        loop {
            let wanted = self.wanted.load(Ordering::SeqCst);
            if wanted > covered {
                self.advance(&mut machine, wanted).await?;
                covered = wanted;
                self.publish(&machine, covered);
            }
            let handoff = self.handoff.load(Ordering::SeqCst);
            if handoff <= covered
                && handoff >= machine.relevant_through
                && machine.relevant_through > machine.saved_through
                && machine.result_id.is_some()
            {
                self.cover(&mut machine).await?;
                self.publish(&machine, covered);
                continue;
            }
            if self.wanted.load(Ordering::SeqCst) <= covered {
                self.wake.notified().await;
            }
        }
    }

    pub(super) async fn fields(
        &self,
        id: &str,
        through: u64,
        fields: &[&str],
    ) -> Result<Vec<QueryValue>> {
        let QueryOutput::Rows { mut rows } = self
            .timeline
            .query(chain_by_id(
                id,
                through,
                QuerySelect::Fields {
                    fields: fields.iter().map(|field| (*field).into()).collect(),
                },
            ))
            .await?
            .result
        else {
            unreachable!("usage source properties")
        };
        rows.pop()
            .map(|row| row.values)
            .ok_or_else(|| fault("missing usage source"))
    }

    async fn newest(&self, mut query: TimelineQuery) -> Result<Option<Vec<QueryValue>>> {
        query.query.pick = Some(QueryPick::Last);
        query.page.as_mut().expect("last usage property page").limit = NonZeroU64::MIN;
        let QueryOutput::Rows { mut rows } = self.timeline.query(query).await?.result else {
            unreachable!("last usage properties")
        };
        Ok(rows.pop().map(|row| row.values))
    }

    async fn restore(&self, through: u64) -> Result<Machine> {
        let branches: Vec<_> = self
            .timeline
            .ancestors()
            .into_iter()
            .map(|(branch, _)| branch)
            .chain(std::iter::once(self.timeline.branch_id()))
            .collect();
        let saved = self
            .newest(request(
                TimelineQueryScope::Chain,
                through,
                &[RESULT],
                &["id", "position", "data"],
            ))
            .await?;
        if let Some(row) = saved {
            let [id, position, data] = <[_; 3]>::try_from(row).expect("usage recovery fields");
            let id: String = decode(id)?;
            let position: TimelinePosition = decode(position)?;
            let data: Value = decode(data)?;
            let mut saved = match data.get("schema").and_then(Value::as_str) {
                Some("context-usage/v1" | "context-usage/v2" | "context-usage/v3") => {
                    legacy::restore(data, &id)?
                }
                Some("context-usage/v4" | SCHEMA) => {
                    let saved: Recovered =
                        serde_json::from_value(data).map_err(|error| fault(error.to_string()))?;
                    if saved.schema == "context-usage/v4" && saved.state.legacy_result.is_some() {
                        return Err(fault("legacy reference in native v4 statistics"));
                    }
                    saved
                }
                _ => return Err(fault("unsupported usage recovery schema")),
            };
            let saved_branch = branches
                .iter()
                .position(|branch| *branch == position.branch_id)
                .ok_or_else(|| fault("numeric result outside visible branches"))?;
            if saved.state.branches != branches[..=saved_branch]
                || saved.state.epoch.is_empty()
                || saved
                    .state
                    .legacy_result
                    .as_ref()
                    .is_some_and(String::is_empty)
                || saved.source_through >= position.sequence
                || saved.source_through
                    > if saved_branch + 1 == branches.len() {
                        through
                    } else {
                        self.timeline.segments(through)?[saved_branch].1
                    }
                || saved.state.initialized > saved.state.at(saved.source_through)
                || saved
                    .state
                    .current
                    .as_ref()
                    .is_some_and(|point| point.at > saved.state.at(saved.source_through))
            {
                return Err(fault("invalid usage recovery boundary"));
            }
            let inherited = position.branch_id != self.timeline.branch_id();
            saved.state.branches = branches;
            let numeric = Numeric::new(
                self.timeline.clone(),
                through,
                saved.state.epoch.clone(),
                saved.state.legacy_result.clone(),
                saved.state.initialized,
            );
            numeric
                .validate_current(saved.state.current.as_ref())
                .await?;
            total(&saved.state.summary())?;
            let configuration = saved.state.summary().configuration;
            let event_id = match &configuration {
                Some(Configuration::Ready { counter }) => Some(counter.event_id.as_str()),
                Some(
                    Configuration::Unsupported { event_id }
                    | Configuration::Unavailable { event_id, .. },
                ) => Some(event_id.as_str()),
                None => None,
            };
            let mut unavailable = None;
            let model = if let Some(id) = event_id {
                let request = self.counter_request(id, through).await?;
                if let Some(Configuration::Ready { counter }) = saved
                    .state
                    .current
                    .as_mut()
                    .and_then(|point| point.totals.configuration.as_mut())
                {
                    if counter.control_identity.is_none() {
                        counter.control_identity =
                            Some(control_identity(&request, &counter.algorithm)?);
                    }
                    if counter.configured_tools.is_none() {
                        counter.configured_tools = Some(self.configured_tools(id, through).await?);
                    }
                }
                let resolved = self
                    .resolver
                    .0
                    .as_ref()
                    .expect("bound usage resolver")
                    .resolve(Arc::new(request), self.cancel.clone())
                    .await;
                let matching = match (&configuration, &resolved) {
                    (Some(Configuration::Ready { counter }), Ok(model)) => model
                        .contribution_counter()
                        .is_some_and(|current| current.algorithm() == counter.algorithm),
                    (Some(Configuration::Unsupported { .. }), Ok(model)) => {
                        model.contribution_counter().is_none()
                    }
                    (Some(Configuration::Unavailable { .. }), Err(_)) => true,
                    _ => false,
                };
                if !matching {
                    match (&configuration, &resolved) {
                        (Some(Configuration::Ready { .. }), Err(error)) => {
                            unavailable = Some(error.clone());
                        }
                        (Some(Configuration::Unavailable { .. }), Ok(_)) => {
                            let mut machine =
                                self.initialize(through, saved.state.branches).await?;
                            self.save(&mut machine).await?;
                            return Ok(machine);
                        }
                        _ => return Err(fault("restored context counter changed its algorithm")),
                    }
                }
                resolved.ok()
            } else {
                None
            };
            let mut machine = Machine {
                state: saved.state,
                source_through: if inherited { 0 } else { saved.source_through },
                result: if inherited { 0 } else { position.sequence },
                result_id: Some(id),
                saved_through: if inherited { 0 } else { saved.source_through },
                relevant_through: if inherited { 0 } else { saved.source_through },
                model: model.clone(),
                unavailable,
                empty: model
                    .as_ref()
                    .and_then(|model| model.contribution_counter())
                    .map(|counter| {
                        let empty = counter.fixed(None, None);
                        (empty, counter.estimate(empty))
                    })
                    .unwrap_or_default(),
                numeric,
            };
            if !inherited
                && let Some(row) = self
                    .newest(request(
                        TimelineQueryScope::Branch,
                        through,
                        &[COVERED],
                        &["position", "data"],
                    ))
                    .await?
            {
                let [position, data] = <[_; 2]>::try_from(row).expect("usage covered fields");
                let position: TimelinePosition = decode(position)?;
                let covered: Covered = decode(data)?;
                if covered.schema != "context-usage-covered/v1"
                    || covered.source_through >= position.sequence
                {
                    return Err(fault("invalid usage covered record"));
                }
                if machine.result_id.as_ref() == Some(&covered.result_id)
                    && covered.source_through > machine.source_through
                {
                    machine.source_through = covered.source_through;
                    machine.saved_through = covered.source_through;
                }
            }
            self.advance(&mut machine, through).await?;
            self.cover(&mut machine).await?;
            if let (Some(Configuration::Ready { mut counter }), Some(model)) =
                (machine.state.summary().configuration, &machine.model)
            {
                let limits = Limits {
                    window: model.context_window(),
                    output: model.output_tokens(),
                    threshold: model.threshold_tokens(),
                };
                if [limits.window, limits.output, limits.threshold]
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.)
                {
                    return Err(fault("invalid restored usage limits"));
                }
                if limits != counter.limits {
                    counter.limits = limits;
                    let at = machine.state.at(through);
                    self.record(
                        &mut machine,
                        at,
                        Summary {
                            configuration: Some(Configuration::Ready { counter }),
                            ..Default::default()
                        },
                        true,
                    )
                    .await?;
                    machine.source_through = through;
                    self.save(&mut machine).await?;
                }
            }
            return Ok(machine);
        }
        let mut machine = self.initialize(through, branches).await?;
        if through > 0 {
            self.save(&mut machine).await?
        }
        Ok(machine)
    }

    async fn configuration_request(&self, id: &str, through: u64) -> Result<ModelRequest> {
        let [settings, system] = <[_; 2]>::try_from(
            self.fields(id, through, &["data.settings", "data.system"])
                .await?,
        )
        .expect("configuration fields");
        let settings: ModelConfiguration = decode(settings)?;
        Ok(ModelSettings {
            provider_id: settings.provider_id,
            model_id: settings.model_id,
            capability_snapshot: settings.capability_snapshot.clone(),
            config: crate::context_model::adopted_config(
                settings.capability_snapshot.as_ref(),
                settings.config.as_ref(),
            ),
            metadata: settings.metadata,
            system: optional(system)?,
            tools: settings.tools,
        }
        .request(vec![]))
    }

    pub(super) async fn configured_tools(&self, id: &str, through: u64) -> Result<bool> {
        let mut query = chain_by_id(id, through, QuerySelect::Exists);
        query.page = None;
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                query.query.filter.take().expect("configuration identity"),
                QueryFilter::Present {
                    value: QueryOperand::Field {
                        path: "data.settings.tools".into(),
                    },
                },
            ],
        });
        let QueryOutput::Exists { exists } = self.timeline.query(query).await?.result else {
            unreachable!("configured tools presence")
        };
        Ok(exists)
    }

    pub(super) async fn counter_request(&self, id: &str, through: u64) -> Result<ModelRequest> {
        let prefix = "data.settings";
        let paths = [
            "providerId",
            "modelId",
            "capabilitySnapshot",
            "config",
            "metadata",
        ]
        .map(|field| format!("{prefix}.{field}"));
        let mut fields: Vec<_> = paths.iter().map(String::as_str).collect();
        fields.push("kind");
        let [provider, model, capabilities, config, metadata, kind] =
            <[_; 6]>::try_from(self.fields(id, through, &fields).await?)
                .expect("counter model controls");
        if decode::<String>(kind)? != "context/configured" {
            return Err(fault("usage configuration references another source type"));
        }
        let mut request = ModelRequest::new(
            decode::<String>(provider)?,
            decode::<String>(model)?,
            vec![],
        );
        request.capability_snapshot = optional(capabilities)?;
        request.config = optional(config)?;
        request.metadata = optional(metadata)?;
        request.config = crate::context_model::adopted_config(
            request.capability_snapshot.as_ref(),
            request.config.as_ref(),
        );
        Ok(request)
    }

    async fn configure(
        &self,
        machine: &mut Machine,
        id: String,
        request: ModelRequest,
    ) -> Result<Configuration> {
        let control = ModelRequest {
            provider_id: request.provider_id.clone(),
            model_id: request.model_id.clone(),
            capability_snapshot: request.capability_snapshot.clone(),
            messages: Vec::new(),
            tools: None,
            system: None,
            config: request.config.clone(),
            metadata: request.metadata.clone(),
        };
        let model = match self
            .resolver
            .0
            .as_ref()
            .expect("bound usage resolver")
            .resolve(Arc::new(control.clone()), self.cancel.clone())
            .await
        {
            Ok(model) => model,
            Err(error) => {
                machine.model = None;
                return Ok(Configuration::Unavailable {
                    event_id: id,
                    message: error.to_string(),
                });
            }
        };
        let configuration = match model.contribution_counter() {
            None => Configuration::Unsupported { event_id: id },
            Some(counter) => {
                let limits = Limits {
                    window: model.context_window(),
                    output: model.output_tokens(),
                    threshold: model.threshold_tokens(),
                };
                if [limits.window, limits.output, limits.threshold]
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.)
                    || counter.algorithm().is_empty()
                {
                    return Err(fault("invalid usage counting contract"));
                }
                let empty = counter.fixed(None, None);
                machine.empty = (empty, counter.estimate(empty));
                let identity = request_identity(&request, counter.algorithm())?;
                let previous = machine.state.summary().configuration;
                let fixed = match previous {
                    Some(Configuration::Ready { counter: previous })
                        if previous.request_identity == identity =>
                    {
                        previous.fixed
                    }
                    _ if request.system.is_none() && request.tools.is_none() => empty,
                    _ => counter.fixed(
                        request.system.as_deref(),
                        request.tools.as_deref().map(Vec::as_slice),
                    ),
                };
                Configuration::Ready {
                    counter: Box::new(CounterConfiguration {
                        event_id: id,
                        request_identity: identity,
                        control_identity: Some(control_identity(&request, counter.algorithm())?),
                        configured_tools: Some(request.tools.is_some()),
                        algorithm: counter.algorithm().into(),
                        fixed,
                        limits,
                    }),
                }
            }
        };
        machine.model = Some(model);
        Ok(configuration)
    }

    async fn initialize(&self, through: u64, branches: Vec<String>) -> Result<Machine> {
        let epoch = Uuid::new_v4().to_string();
        let initialized = Key {
            branch: branches.len() as u32 - 1,
            sequence: through,
        };
        let mut machine = Machine {
            state: State {
                epoch: epoch.clone(),
                branches,
                initialized,
                legacy_result: None,
                current: None,
            },
            source_through: through,
            result: 0,
            result_id: None,
            saved_through: 0,
            relevant_through: 0,
            model: None,
            unavailable: None,
            empty: Default::default(),
            numeric: Numeric::new(self.timeline.clone(), through, epoch, None, initialized),
        };
        let Some(mut row) = self
            .newest(request(
                TimelineQueryScope::Chain,
                through,
                &["context/configured"],
                &["id"],
            ))
            .await?
        else {
            return Ok(machine);
        };
        let id: String = decode(row.remove(0))?;
        let value = self.configuration_request(&id, through).await?;
        let configuration = self.configure(&mut machine, id, value).await?;
        let ready = matches!(configuration, Configuration::Ready { .. });
        let mut base = Summary {
            configuration: Some(configuration),
            ..Default::default()
        };
        let mut compaction = request(
            TimelineQueryScope::Chain,
            through,
            &["context/compaction"],
            &[
                "id",
                "data.retain_from",
                "data.replacement",
                "data.context_baseline",
            ],
        );
        compaction.query.filter = Some(QueryFilter::And {
            conditions: vec![
                compaction.query.filter.take().expect("compaction kind"),
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "data.status".into(),
                    },
                    values: vec!["completed".into()],
                },
            ],
        });
        let mut from = None;
        if ready && let Some(row) = self.newest(compaction).await? {
            let [id, cut, replacement, baseline] =
                <[_; 4]>::try_from(row).expect("initialized compression fields");
            let id: String = decode(id)?;
            let cut_id: String = decode(cut)?;
            let mut position = self.fields(&cut_id, through, &["position"]).await?;
            let position: TimelinePosition = decode(position.remove(0))?;
            let replacement: Vec<Message> = decode(replacement)?;
            let baseline: Vec<ContextBaselineEntry> = optional(baseline)?.unwrap_or_default();
            let counter = machine
                .model
                .as_ref()
                .and_then(|model| model.contribution_counter())
                .expect("ready initialized counter");
            let mut contribution = Contribution::default();
            for message in replacement
                .iter()
                .chain(baseline.iter().map(|entry| entry.message.as_ref()))
            {
                contribution = add(contribution, counter.message(message))?;
            }
            base.compression = Some(Compression {
                event_id: id,
                cut: machine.state.key(&position)?,
                removed: Contribution::default(),
                replacement: contribution,
                removed_messages: Some(0),
                replacement_messages: Some((replacement.len() + baseline.len()) as u64),
            });
            from = Some(position);
        }
        if ready {
            self.count_history(
                &machine,
                through,
                from.as_ref()
                    .map(|position| machine.state.key(position))
                    .transpose()?,
                None,
                &mut base,
            )
            .await?;
            let counter = machine
                .model
                .as_ref()
                .and_then(|model| model.contribution_counter())
                .expect("ready initial counter");
            base.local = Some(counter.estimate(total(&base)?));
        }
        machine.state.current = Some(
            machine
                .numeric
                .append(
                    None,
                    Entry {
                        at: initialized,
                        summary: base,
                    },
                )
                .await?,
        );
        Ok(machine)
    }

    async fn count_history(
        &self,
        machine: &Machine,
        through: u64,
        from: Option<Key>,
        before: Option<Key>,
        summary: &mut Summary,
    ) -> Result<()> {
        let counter = machine
            .model
            .as_ref()
            .and_then(|model| model.contribution_counter())
            .ok_or_else(|| fault("missing initialization counter"))?;
        let mut read = crate::history::FoldRead::timeline(&self.timeline, None, through);
        let mut state = None;
        while let Some(page) = read.next(256).await? {
            let mut begin = 0;
            while begin < page.events.len() {
                let branch = &page.positions[begin].branch_id;
                let end = (begin + 1..page.events.len())
                    .find(|index| page.positions[*index].branch_id != *branch)
                    .unwrap_or(page.events.len());
                let projected =
                    crate::history::project_batch(&page.events[begin..end], state.as_ref(), branch)
                        .map_err(|error| fault(error.to_string()))?;
                state = Some(projected.state);
                for entry in projected.history {
                    let at = machine.state.key(&entry.position)?;
                    if before.is_some_and(|end| at >= end) {
                        return Ok(());
                    }
                    if from.is_none_or(|start| at >= start) {
                        message(summary, counter, machine.empty, &entry.message)?;
                        summary.last_entry = Some(LastEntry {
                            at,
                            id: Some(entry.id),
                        });
                    }
                }
                begin = end;
            }
        }
        Ok(())
    }

    async fn record(
        &self,
        machine: &mut Machine,
        at: Key,
        mut entry: Summary,
        estimate: bool,
    ) -> Result<()> {
        if entry.ordinary == Contribution::default()
            && entry.configuration.is_none()
            && entry.compression.is_none()
            && entry.calibration.is_none()
            && entry.last_entry.is_none()
        {
            return Ok(());
        }
        if estimate {
            let mut next = machine.state.summary();
            next.join(&entry)?;
            if matches!(next.configuration, Some(Configuration::Ready { .. })) {
                let counter = machine
                    .model
                    .as_ref()
                    .and_then(|model| model.contribution_counter())
                    .ok_or_else(|| fault("missing ready usage counter"))?;
                let local = counter.estimate(total(&next)?);
                if !local.is_finite() || local < 0. {
                    return Err(fault("invalid local usage estimate"));
                }
                entry.local = Some(local);
            }
        }
        machine.state.current = Some(
            machine
                .numeric
                .append(machine.state.current.take(), Entry { at, summary: entry })
                .await?,
        );
        Ok(())
    }

    async fn advance(&self, machine: &mut Machine, through: u64) -> Result<()> {
        if machine.unavailable.is_some() || machine.source_through >= through {
            return Ok(());
        }
        let mut sources = request(
            TimelineQueryScope::Branch,
            through,
            SOURCES,
            &["id", "kind", "position"],
        );
        sources.query.filter = Some(QueryFilter::And {
            conditions: vec![
                sources.query.filter.take().expect("usage source kinds"),
                QueryFilter::Or {
                    conditions: vec![
                        QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "context.render".into(),
                            },
                            values: vec![true.into()],
                        },
                        QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "kind".into(),
                            },
                            values: vec!["context/configured".into(), "model/usage".into()],
                        },
                    ],
                },
            ],
        });
        sources.from = Some(TimelinePosition {
            branch_id: self.timeline.branch_id(),
            sequence: machine.source_through + 1,
        });
        sources.query.order_by = vec![QueryOrder {
            key: QueryOrderKey::Position,
            direction: TimelineQueryDirection::Ascending,
        }];
        loop {
            let reply = self.timeline.query(sources.clone()).await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("usage source identities")
            };
            if rows.is_empty() {
                break;
            }
            for row in rows {
                let [id, kind, position] =
                    <[_; 3]>::try_from(row.values).expect("usage source identity");
                let id: String = decode(id)?;
                let kind: String = decode(kind)?;
                let position: TimelinePosition = decode(position)?;
                self.consume(machine, &id, &kind, position.clone(), through)
                    .await?;
                machine.source_through = position.sequence;
                machine.relevant_through = position.sequence;
            }
            self.save(machine).await?;
            let Some(next) = reply.next else { break };
            sources.page.as_mut().unwrap().after = Some(next);
        }
        machine.source_through = through;
        Ok(())
    }

    async fn consume(
        &self,
        machine: &mut Machine,
        id: &str,
        kind: &str,
        position: TimelinePosition,
        through: u64,
    ) -> Result<()> {
        let at = machine.state.key(&position)?;
        if kind == "context/configured" {
            let request = self.configuration_request(id, through).await?;
            let previous = machine.state.summary().configuration;
            let next = self.configure(machine, id.into(), request).await?;
            let same_algorithm = match (&previous, &next) {
                (
                    Some(Configuration::Ready { counter: old }),
                    Configuration::Ready { counter: new },
                ) => old.algorithm == new.algorithm,
                _ => false,
            };
            if !same_algorithm && matches!(next, Configuration::Ready { .. }) {
                if matches!(previous, Some(Configuration::Ready { .. })) {
                    return Err(fault("context contribution algorithm changed"));
                }
                *machine =
                    Box::pin(self.initialize(position.sequence, machine.state.branches.clone()))
                        .await?;
                return Ok(());
            }
            if match (&previous, &next) {
                (
                    Some(Configuration::Ready { counter: previous }),
                    Configuration::Ready { counter: next },
                ) => {
                    previous.request_identity == next.request_identity
                        && previous.limits == next.limits
                }
                _ => previous.as_ref() == Some(&next),
            } {
                return Ok(());
            }
            return self
                .record(
                    machine,
                    at,
                    Summary {
                        configuration: Some(next),
                        ..Default::default()
                    },
                    true,
                )
                .await;
        }
        if kind == "model/usage" {
            return self.calibrate(machine, id, at, through).await;
        }
        let Some(model) = machine.model.clone() else {
            return Ok(());
        };
        let Some(counter) = model.contribution_counter() else {
            return Ok(());
        };
        let mut entry = Summary::default();
        match kind {
            "input/consumed" => {
                let mut fields = self.fields(id, through, &["data.input_id"]).await?;
                let input: String = decode(fields.remove(0))?;
                let mut fields = self.fields(&input, through, &["data"]).await?;
                let input: InputMessageData = decode(fields.remove(0))?;
                for projection in render_input(id, &input) {
                    message(&mut entry, counter, machine.empty, &projection.message)?;
                    entry.last_entry = Some(LastEntry {
                        at,
                        id: Some(projection.id),
                    });
                }
            }
            "step/model-output-end" => {
                let [request, status] = <[_; 2]>::try_from(
                    self.fields(id, through, &["data.request_id", "data.status"])
                        .await?,
                )
                .expect("output settlement fields");
                let request: String = decode(request)?;
                let status: String = decode(status)?;
                if let Some(output) = settled_request_output(
                    &self.timeline,
                    &request,
                    through,
                    &position,
                    status == "completed",
                    false,
                )
                .await?
                {
                    message(&mut entry, counter, machine.empty, &output)?;
                    entry.last_entry = Some(LastEntry {
                        at,
                        id: Some(id.into()),
                    });
                }
            }
            "step/tool-result" => {
                let [request, call, calls, contents, errors, contexts] = <[_; 6]>::try_from(
                    self.fields(
                        id,
                        through,
                        &[
                            "data.request_id",
                            "data.call_event_id",
                            "data.results[].call_id",
                            "data.results[].result.content",
                            "data.results[].result.isError",
                            "data.results[].result.contextMessages",
                        ],
                    )
                    .await?,
                )
                .expect("tool result properties");
                let request: String = decode(request)?;
                if optional::<String>(call)?.is_none()
                    && let Some(output) = settled_request_output(
                        &self.timeline,
                        &request,
                        through,
                        &position,
                        false,
                        true,
                    )
                    .await?
                {
                    message(&mut entry, counter, machine.empty, &output)?;
                    entry.last_entry = Some(LastEntry {
                        at,
                        id: Some(id.into()),
                    });
                }
                let calls = elements(calls)?
                    .into_iter()
                    .map(decode::<String>)
                    .collect::<Result<Vec<_>>>()?;
                let contents = elements(contents)?
                    .into_iter()
                    .map(decode::<ToolContent>)
                    .collect::<Result<Vec<_>>>()?;
                let errors = elements(errors)?
                    .into_iter()
                    .map(optional::<bool>)
                    .collect::<Result<Vec<_>>>()?;
                let contexts = elements(contexts)?
                    .into_iter()
                    .map(optional::<Vec<Message>>)
                    .collect::<Result<Vec<_>>>()?;
                if [contents.len(), errors.len(), contexts.len()]
                    .iter()
                    .any(|length| *length != calls.len())
                {
                    return Err(fault("unaligned tool result properties"));
                }
                for (index, (((call, content), error), context)) in calls
                    .into_iter()
                    .zip(contents)
                    .zip(errors)
                    .zip(contexts)
                    .enumerate()
                {
                    message(
                        &mut entry,
                        counter,
                        machine.empty,
                        &tool_result_message(&call, &content, error),
                    )?;
                    entry.last_entry = Some(LastEntry {
                        at,
                        id: Some(format!("{id}:{index}")),
                    });
                    for (context_index, context) in context.into_iter().flatten().enumerate() {
                        message(&mut entry, counter, machine.empty, &context)?;
                        entry.last_entry = Some(LastEntry {
                            at,
                            id: Some(format!("{id}:{index}:{context_index}")),
                        });
                    }
                }
            }
            "context/compaction" => {
                let mut fields = self.fields(id, through, &["data.status"]).await?;
                if decode::<String>(fields.remove(0))? != "completed" {
                    return Ok(());
                }
                let [cut, replacement, baseline, prepared] = <[_; 4]>::try_from(
                    self.fields(
                        id,
                        through,
                        &[
                            "data.retain_from",
                            "data.replacement",
                            "data.context_baseline",
                            "data.through",
                        ],
                    )
                    .await?,
                )
                .expect("compaction properties");
                let cut: String = decode(cut)?;
                let mut fields = self.fields(&cut, through, &["position"]).await?;
                let cut: TimelinePosition = decode(fields.remove(0))?;
                let cut = machine.state.key(&cut)?;
                let current = machine.state.summary();
                if current
                    .compression
                    .as_ref()
                    .is_some_and(|previous| previous.cut > cut)
                {
                    return Err(fault("compaction cut precedes the active cut"));
                }
                let replacement: Vec<Message> = decode(replacement)?;
                let baseline: Vec<ContextBaselineEntry> = optional(baseline)?.unwrap_or_default();
                let mut contribution = Contribution::default();
                for message in replacement
                    .iter()
                    .chain(baseline.iter().map(|entry| entry.message.as_ref()))
                {
                    contribution = add(contribution, counter.message(message))?;
                }
                let prefix = match machine
                    .numeric
                    .prefix(machine.state.current.as_ref(), cut, false)
                    .await?
                {
                    Some(prefix) => prefix,
                    None => {
                        let mut prefix = Summary::default();
                        if let Some(previous) = &current.compression {
                            prefix.ordinary = previous.removed;
                            prefix.message_count = previous.removed_messages;
                        }
                        self.count_history(
                            machine,
                            decode(prepared)?,
                            current.compression.as_ref().map(|previous| previous.cut),
                            Some(cut),
                            &mut prefix,
                        )
                        .await?;
                        prefix
                    }
                };
                let removed = prefix.ordinary;
                if current
                    .last_entry
                    .as_ref()
                    .is_some_and(|last| last.id.is_some() && last.at < cut)
                {
                    entry.last_entry = Some(LastEntry { at, id: None });
                }
                entry.compression = Some(Compression {
                    event_id: id.into(),
                    cut,
                    removed,
                    replacement: contribution,
                    removed_messages: prefix.message_count,
                    replacement_messages: Some((replacement.len() + baseline.len()) as u64),
                });
            }
            _ => unreachable!("declared usage source"),
        }
        self.record(machine, at, entry, true).await
    }

    async fn calibrate(
        &self,
        machine: &mut Machine,
        id: &str,
        at: Key,
        through: u64,
    ) -> Result<()> {
        let [request_id, prompt] = <[_; 2]>::try_from(
            self.fields(id, through, &["data.request_id", "data.usage"])
                .await?,
        )
        .expect("provider usage properties");
        let request_id: String = decode(request_id)?;
        let usage: Usage = decode(prompt)?;
        self.delivery(at.sequence).await?;
        let candidate = self
            .association
            .lock()
            .unwrap()
            .measurements
            .remove(&at.sequence);
        let Some(prompt_tokens) = measured_prompt(&usage) else {
            return Ok(());
        };
        let summary = machine.state.summary();
        let Some(Configuration::Ready { counter }) = &summary.configuration else {
            return Ok(());
        };
        if let Some(mut calibration) = summary
            .calibration
            .clone()
            .filter(|current| current.request_id == request_id)
        {
            if calibration.prompt_tokens == prompt_tokens {
                return Ok(());
            }
            calibration.prompt_tokens = prompt_tokens;
            return self
                .record(
                    machine,
                    at,
                    Summary {
                        calibration: Some(calibration),
                        ..Default::default()
                    },
                    false,
                )
                .await;
        }
        let Some(candidate) = candidate else {
            return Ok(());
        };
        let request_at = machine.state.key(&candidate.source)?;
        let anchor_at = machine.state.key(&candidate.anchor)?;
        if request_at < machine.state.initialized
            || summary.calibration.as_ref().is_some_and(|current| {
                (current.request_at, current.anchor_at) > (request_at, anchor_at)
            })
        {
            return Ok(());
        }
        let basis = &candidate.calibration;
        if basis.configuration != counter.identity()?
            || basis.algorithm != counter.algorithm
            || basis.compression.as_deref()
                != summary
                    .compression
                    .as_ref()
                    .map(|compression| compression.event_id.as_str())
        {
            return Ok(());
        }
        self.record(
            machine,
            at,
            Summary {
                calibration: Some(Calibration {
                    request_id,
                    request_at,
                    anchor_at,
                    configuration: basis.configuration.clone(),
                    compression: basis.compression.clone(),
                    request_identity: basis.request_identity.clone(),
                    local: basis.local,
                    prompt_tokens,
                }),
                ..Default::default()
            },
            false,
        )
        .await
    }
}

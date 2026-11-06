//! ToolBinding: a branch's adopted tool set and each request's tools, see
//! `docs/architecture/rust-agent/plugins/ToolExecution.md#toolbinding` and
//! `tool-contributions.md` §7.
//!
//! The branch adopts the registry's current tool set as the tool part of its
//! configuration, pins the source sets its prefix may still declare, and
//! fixes each request's handlers when Loop binds it, so a call always runs
//! the implementation of the version its request declared.
use crate::{
    context_preparation::ToolSetAdoption,
    history::context_configuration,
    host::ModelSettings,
    prompt::{Prompt, ToolAdoptOutcome},
    tool_registry::{ToolContribution, ToolRegistry, ToolSourceSet},
};
use dim_agent_runtime::{AgentError, BranchLifecycle, BranchOpening, Dispatcher, Names, Timeline};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

struct Snapshot {
    tools: HashMap<String, Arc<ToolContribution>>,
    /// Keeps the versions this request declared alive until it is released.
    _sets: Vec<Arc<ToolSourceSet>>,
}

#[derive(Default)]
struct State {
    /// Sets this branch adopted at a local sequence (0: before this
    /// generation) that a later request prefix may still declare.
    pins: Vec<(u64, Vec<Arc<ToolSourceSet>>)>,
    /// The tool part this branch adopted last.
    adopted: Option<ToolSetAdoption>,
    /// Whether the adopted configuration carries a tool part. Without one,
    /// each request declares the registry's current host and contributed
    /// tools (`bind_current`), so only another source's tools need adoption.
    declares_tools: bool,
    snapshots: HashMap<String, Snapshot>,
}

pub struct ToolBinding {
    registry: Arc<ToolRegistry>,
    state: Mutex<State>,
    changed: tokio::sync::Notify,
}

/// The registry's tool part and the sets it was built from.
fn current(registry: &ToolRegistry) -> (ToolSetAdoption, Vec<Arc<ToolSourceSet>>) {
    let (tools, sets) = registry.snapshot();
    let adoption = ToolSetAdoption {
        tools,
        tool_sources: sets
            .iter()
            .map(|set| ToolSourceVersion {
                key: set.key.clone(),
                version: set.version.clone(),
            })
            .collect(),
    };
    (adoption, sets)
}

impl ToolBinding {
    /// Fixes the handlers of `request_id` to the tools its prefix (through
    /// `through`) adopted, then releases the pins no later prefix declares.
    pub fn bind(
        &self,
        request_id: &str,
        through: u64,
        tools: &[ToolDefinition],
        tool_sources: &[ToolSourceVersion],
    ) -> Result<(), String> {
        let sets = tool_sources
            .iter()
            .map(|source| {
                self.registry
                    .source(&source.key, &source.version)
                    .ok_or_else(|| {
                        format!(
                            "tool source {}@{} is unavailable",
                            source.key, source.version
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut bound = HashMap::new();
        for tool in tools {
            let handler = sets
                .iter()
                .flat_map(|set| set.tools.iter())
                .find(|candidate| candidate.definition.name == tool.name)
                .cloned()
                .or_else(|| self.registry.fixed(&tool.name))
                .ok_or_else(|| format!("tool {} has no handler", tool.name))?;
            bound.insert(tool.name.clone(), handler);
        }
        let mut state = self.state.lock().unwrap();
        register(&mut state, request_id, bound, sets)?;
        if let Some(newest) = state
            .pins
            .iter()
            .map(|(sequence, _)| *sequence)
            .filter(|sequence| *sequence <= through)
            .max()
        {
            state.pins.retain(|(sequence, _)| *sequence >= newest);
        }
        Ok(())
    }

    /// Binds a request whose configuration declares no tool part: declares
    /// the registry's current tools, or checks `declared` against them.
    /// Returns the declaration.
    pub fn bind_current(
        &self,
        request_id: &str,
        declared: Option<&[ToolDefinition]>,
    ) -> Result<Vec<ToolDefinition>, String> {
        let (definitions, sets) = self.registry.snapshot();
        let tools = declared.unwrap_or(&definitions);
        self.bind_snapshot(request_id, tools, sets)?;
        Ok(tools.to_vec())
    }

    pub(crate) fn prepare(
        &self,
        request_id: &str,
        prepare: impl FnOnce(&[ToolDefinition]) -> dim_agent_runtime::Result<ModelSettings>,
    ) -> dim_agent_runtime::Result<ModelSettings> {
        let (definitions, sets) = self.registry.snapshot();
        let settings = prepare(&definitions)?;
        self.bind_snapshot(
            request_id,
            settings.tools.as_deref().unwrap_or_default(),
            sets,
        )
        .map_err(AgentError::InvalidInput)?;
        Ok(settings)
    }

    fn bind_snapshot(
        &self,
        request_id: &str,
        tools: &[ToolDefinition],
        sets: Vec<Arc<ToolSourceSet>>,
    ) -> Result<(), String> {
        let mut bound = HashMap::new();
        for tool in tools {
            let handler = self
                .registry
                .fixed(&tool.name)
                .filter(|candidate| candidate.definition == *tool)
                .or_else(|| {
                    sets.iter()
                        .flat_map(|set| set.tools.iter())
                        .find(|candidate| candidate.definition == *tool)
                        .cloned()
                })
                .ok_or_else(|| format!("tool {} has no handler", tool.name))?;
            bound.insert(tool.name.clone(), handler);
        }
        register(&mut self.state.lock().unwrap(), request_id, bound, sets)
    }

    /// The tool a call of `request_id` runs: its definition (for admission)
    /// and its handler, from the request's snapshot only.
    pub fn handler(&self, request_id: &str, name: &str) -> Result<Arc<ToolContribution>, String> {
        let state = self.state.lock().unwrap();
        let snapshot = state
            .snapshots
            .get(request_id)
            .ok_or_else(|| format!("request {request_id} has no active tool binding"))?;
        snapshot
            .tools
            .get(name)
            .cloned()
            .ok_or_else(|| format!("Unknown tool: {name}"))
    }

    /// Moves a request's snapshot to its budget recovery request.
    pub fn transfer(&self, from: &str, to: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let snapshot = state
            .snapshots
            .remove(from)
            .ok_or_else(|| format!("request {from} has no active tool binding"))?;
        state.snapshots.insert(to.to_owned(), snapshot);
        Ok(())
    }

    pub fn release(&self, request_id: &str) {
        self.state.lock().unwrap().snapshots.remove(request_id);
    }

    /// Keeps only the snapshot of `request_id`.
    pub fn retain_only(&self, request_id: Option<&str>) {
        self.state
            .lock()
            .unwrap()
            .snapshots
            .retain(|id, _| Some(id.as_str()) == request_id);
    }

    /// Matches the adopted configuration's tool part with the registry once a
    /// model configuration exists. Without one the branch prepares requests
    /// through PrepareModelHost and binds them with `bind_current`.
    async fn reconcile(
        &self,
        prompt: &Prompt,
        timeline: &Timeline,
        cancel: &CancellationToken,
    ) -> dim_agent_runtime::Result<()> {
        let (adoption, sets) = current(&self.registry);
        let prefix = crate::history::HistoryRenderer::default()
            .context(timeline, timeline.sequence())
            .await?;
        let Some(configuration) = context_configuration(&prefix) else {
            return Ok(());
        };
        let declared = match &configuration.settings.tools {
            Some(tools) => {
                *tools == adoption.tools
                    && configuration.settings.tool_sources == adoption.tool_sources
            }
            None => only_contributed(&adoption),
        };
        self.state.lock().unwrap().declares_tools = configuration.settings.tools.is_some();
        if declared {
            let mut state = self.state.lock().unwrap();
            if state.adopted.is_none() {
                if state.declares_tools {
                    state.pins.push((0, sets));
                }
                state.adopted = Some(adoption);
            }
            Ok(())
        } else {
            self.adopt(prompt, cancel).await
        }
    }

    /// Adopts the registry's current tool part when it differs from the last
    /// adopted one, and pins its sets at the adoption's sequence.
    async fn adopt(
        &self,
        prompt: &Prompt,
        cancel: &CancellationToken,
    ) -> dim_agent_runtime::Result<()> {
        let (adoption, sets) = current(&self.registry);
        {
            let mut state = self.state.lock().unwrap();
            if state.adopted.as_ref() == Some(&adoption) {
                return Ok(());
            }
            if !state.declares_tools && only_contributed(&adoption) {
                state.adopted = Some(adoption);
                return Ok(());
            }
        }
        match prompt.adopt_tools(adoption.clone(), cancel.clone()).await? {
            ToolAdoptOutcome::Adopted { sequence } => {
                let mut state = self.state.lock().unwrap();
                state.pins.push((sequence, sets));
                state.adopted = Some(adoption);
                state.declares_tools = true;
            }
            ToolAdoptOutcome::Unchanged => {
                let mut state = self.state.lock().unwrap();
                state.pins.push((0, sets));
                state.adopted = Some(adoption);
            }
            ToolAdoptOutcome::Superseded => {}
        }
        Ok(())
    }
}

fn only_contributed(adoption: &ToolSetAdoption) -> bool {
    adoption
        .tool_sources
        .iter()
        .all(|source| crate::tool_registry::is_contributed(&source.key))
}

fn register(
    state: &mut State,
    request_id: &str,
    tools: HashMap<String, Arc<ToolContribution>>,
    sets: Vec<Arc<ToolSourceSet>>,
) -> Result<(), String> {
    if state.snapshots.contains_key(request_id) {
        return Err(format!("request {request_id} is already bound"));
    }
    state
        .snapshots
        .insert(request_id.to_owned(), Snapshot { tools, _sets: sets });
    Ok(())
}

pub struct ToolBindingPlugin {
    binding: Key<ToolBinding>,
    registry: Key<ToolRegistry>,
    prompt: Key<Prompt>,
    timeline: Key<Timeline>,
    phase: Key<BranchLifecycle>,
    dispatcher: Key<Dispatcher>,
    opening: Key<BranchOpening>,
}
impl ToolBindingPlugin {
    pub fn new(names: &Names<'_>) -> std::result::Result<Self, Error> {
        Ok(Self {
            binding: names.key()?,
            registry: names.key()?,
            prompt: names.key()?,
            timeline: names.key()?,
            phase: names.key()?,
            dispatcher: names.key()?,
            opening: names.key()?,
        })
    }
}
impl DynPlugin for ToolBindingPlugin {
    fn name(&self) -> &str {
        "ToolBinding"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.registry.erase(),
            self.prompt.erase(),
            self.timeline.erase(),
            self.phase.erase(),
            self.dispatcher.erase(),
            self.opening.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.registry)?;
            let prompt = ctx.require_at(&self.prompt)?;
            let timeline = ctx.require_at(&self.timeline)?;
            let phase = ctx.require_at(&self.phase)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let opening = ctx.require_at(&self.opening)?;
            let binding = Arc::new(ToolBinding {
                registry: registry.clone(),
                state: Mutex::default(),
                changed: tokio::sync::Notify::new(),
            });
            let lifecycle = ctx.cancellation_token();
            let tasks = TaskTracker::new();
            let opening_tasks = tasks.clone();
            let mut sources = registry.sources_state();
            let (forward, forward_cancel) = (binding.clone(), lifecycle.clone());
            tasks.spawn(async move {
                loop {
                    tokio::select! {
                        _ = forward_cancel.cancelled() => return,
                        changed = sources.changed() => match changed {
                            Ok(()) => forward.changed.notify_one(),
                            Err(_) => return,
                        },
                    }
                }
            });
            let (owner, opening_prompt, opening_timeline, opening_cancel) = (
                binding.clone(),
                prompt.clone(),
                timeline.clone(),
                lifecycle.clone(),
            );
            dispatcher.subscribe(
                ctx,
                "ToolBinding",
                &["branch/start", "branch/resume"],
                move |_| {
                    let (owner, prompt, timeline, cancel) = (
                        owner.clone(),
                        opening_prompt.clone(),
                        opening_timeline.clone(),
                        opening_cancel.clone(),
                    );
                    let (reply, receipt) = tokio::sync::oneshot::channel();
                    let registered = opening.register(Box::pin(async move {
                        receipt
                            .await
                            .map_err(|_| AgentError::Closed("ToolBinding".into()))?
                    }));
                    if registered.is_ok() {
                        opening_tasks.spawn(async move {
                            let result = AssertUnwindSafe(async {
                                prompt.initial_completion().await?;
                                owner.reconcile(&prompt, &timeline, &cancel).await
                            })
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|panic| {
                                Err(AgentError::fault(
                                    "ToolBinding opening",
                                    crate::host::panic_message(panic),
                                ))
                            });
                            let _ = reply.send(result);
                        });
                    }
                    Box::pin(async move { registered })
                },
            )?;
            binding.reconcile(&prompt, &timeline, &lifecycle).await?;
            let (watcher, cancel) = (binding.clone(), lifecycle.clone());
            tasks.spawn(async move {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    active = phase.activated() => if !active { return },
                }
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = phase.closing() => return,
                        _ = watcher.changed.notified() => {},
                    }
                    if let Err(error) = watcher.adopt(&prompt, &cancel).await {
                        if matches!(error, AgentError::Closed(_)) {
                            return;
                        }
                        tracing::warn!(%error, "tool set adoption failed");
                    }
                }
            });
            ctx.effect(move || async move {
                tasks.close();
                tasks.wait().await;
                Ok(())
            })?;
            ctx.provide_at(self.binding.clone(), binding)?;
            Ok(Effect::Done)
        })
    }
}

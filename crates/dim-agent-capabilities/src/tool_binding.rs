//! ToolBinding: a branch's adopted tool set and each request's tools, see
//! `docs/architecture/rust-agent/plugins/ToolBinding.md` and
//! `tool-contributions.md` §7.
//!
//! The branch adopts the registry's current tool set as the tool part of its
//! configuration, pins the source sets its prefix may still declare, and
//! fixes each request's handlers when Loop binds it, so a call always runs
//! the implementation of the version its request declared.
use crate::{
    context_preparation::ToolSetAdoption,
    history::context_configuration,
    prompt::{Prompt, ToolAdoptOutcome},
    tool_registry::{ToolContribution, ToolRegistry, ToolSourceSet, ToolsChanged},
};
use dim_agent_runtime::{
    AgentError, BranchLifecycle, BranchOpening, BranchScope, Capability, Dispatcher, InBranch,
    InSession, KeysOf, Timeline, scoped,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::{BoxFuture, CordisError, Ctx, Effect, EventKey, Listener, TypedPlugin};
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

impl Listener<ToolsChanged> for Arc<ToolBinding> {
    fn call<'a>(
        &'a self,
        _: &'a Ctx,
        _: &'a ToolsChanged,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        Box::pin(async move {
            self.changed.notify_one();
            Ok(None)
        })
    }
}

/// The registry's current tool part and the sets it was built from.
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
        // Every later request's prefix contains the newest pin at or before
        // `through`, so older pins are no longer declared.
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
    /// the registry's current tools, or checks `declared` (from
    /// `PrepareModelHost`) against them. Returns the declaration.
    pub fn bind_current(
        &self,
        request_id: &str,
        declared: Option<&[ToolDefinition]>,
    ) -> Result<Vec<ToolDefinition>, String> {
        let (definitions, sets) = self.registry.snapshot();
        let available: Vec<Arc<ToolContribution>> = definitions
            .iter()
            .filter_map(|definition| {
                self.registry.fixed(&definition.name).or_else(|| {
                    sets.iter()
                        .flat_map(|set| set.tools.iter())
                        .find(|tool| tool.definition.name == definition.name)
                        .cloned()
                })
            })
            .collect();
        let tools: Vec<ToolDefinition> = match declared {
            None => available
                .iter()
                .map(|tool| tool.definition.clone())
                .collect(),
            Some(declared) => declared.to_vec(),
        };
        let mut bound = HashMap::new();
        for tool in &tools {
            let handler = available
                .iter()
                .find(|candidate| candidate.definition == *tool)
                .cloned()
                .ok_or_else(|| format!("tool {} has no handler", tool.name))?;
            bound.insert(tool.name.clone(), handler);
        }
        register(&mut self.state.lock().unwrap(), request_id, bound, sets)?;
        Ok(tools)
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
        // Fix optional providers during branch initialization even without a model.
        let (adoption, sets) = current(&self.registry);
        let prefix = crate::history::read_context(timeline, timeline.sequence()).await?;
        let Some(configuration) = context_configuration(&prefix) else {
            return Ok(());
        };
        // A configuration without a tool part declares the fixed tools (Loop
        // binds it with `bind_current`); it needs an adoption only once a
        // dynamic source offers tools.
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
                // Without a tool part requests bind the registry's current
                // sets and retain them in their own snapshots; no prefix can
                // name these versions, so they are not pinned.
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
    scope: BranchScope,
}
impl ToolBindingPlugin {
    pub fn new(scope: &BranchScope) -> Self {
        Self { scope: *scope }
    }
}
impl TypedPlugin for ToolBindingPlugin {
    type Deps = (
        InSession<ToolRegistry>,
        InBranch<Prompt>,
        InBranch<Timeline>,
        InBranch<BranchLifecycle>,
        InBranch<Dispatcher>,
        InBranch<BranchOpening>,
    );
    fn name(&self) -> &str {
        "ToolBinding"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (
            InSession(registry),
            InBranch(prompt),
            InBranch(timeline),
            InBranch(phase),
            InBranch(dispatcher),
            InBranch(opening),
        ): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let binding = Arc::new(ToolBinding {
                registry: registry.clone(),
                state: Mutex::default(),
                changed: tokio::sync::Notify::new(),
            });
            let lifecycle = ctx.cancellation_token();
            let tasks = TaskTracker::new();
            let opening_tasks = tasks.clone();
            // Subscribe before reading the registry so no change is missed.
            ctx.events().on(
                ctx,
                &EventKey::<ToolsChanged>::of().instance(self.scope.session.instance),
                binding.clone(),
            )?;
            // A new or resumed branch receives its model configuration while
            // `branch/start` or `branch/resume` dispatches (Prompt, which this
            // follows); adopt the tool part then, before the branch takes input.
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
            // A reloaded ToolBinding finds the configuration already adopted.
            binding.reconcile(&prompt, &timeline, &lifecycle).await?;
            let (watcher, cancel) = (binding.clone(), lifecycle.clone());
            tasks.spawn(async move {
                // Background work runs while the branch is Active; a
                // change before that retains a permit in `changed`.
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
                    // A failed adoption keeps the pins and the prefix; the
                    // next change retries.
                    if let Err(error) = watcher.adopt(&prompt, &cancel).await {
                        if matches!(error, AgentError::Closed(_)) {
                            return;
                        }
                        tracing::warn!(%error, "tool set adoption failed");
                    }
                }
            });
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        tasks.close();
                        tasks.wait().await;
                        Ok(())
                    })
                }))
            })?;
            ctx.provide_as(self.scope.key::<ToolBinding>(), binding)?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for ToolBindingPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

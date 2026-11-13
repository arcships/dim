//! Workspace tools, see `docs/architecture/rust-agent/plugins/WorkspaceTools.md`.
//!
//! `WorkspaceToolsOwner` (process) registers the Session part `WorkspaceTools`,
//! which provides `S:ToolHost` executing `read` `write` `edit` `glob` `grep`
//! `exec` `exec_stop`, and the branch parts `FileReads` and `BackgroundExec`.
//! It holds the file-write coordinator: one lock domain for every Session it
//! serves. Branch parts are looked up per call on the calling branch: `FileReads`
//! for file tools and `BackgroundExec` for a command that moves to the
//! background. Admission happened before a call reaches execution;
//! `resolve_write` only prepares its target before admission. Nothing here
//! decides permissions, approvals or sandboxes.
pub mod background;
pub mod exec;
pub mod file_reads;
pub mod files;
pub mod mentions;
mod pending;

use crate::{
    host::{HostError, HostFuture, SessionFilesHost, ToolDriver, ToolExecutionContext, ToolHost},
    host_platform::HostPlatform,
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params, reply},
    timeline_tree::TreeCommands,
};
use background::BackgroundExecPlugin;
use dim_agent_runtime::{
    AgentBuilder, AgentError, CAPABILITY_RELOADING, CollaborationRole, Names, PartRegistry,
    PartScope, SessionConfig, SessionIdentity,
};
use dim_agent_types::*;
use file_reads::{FileReads, FileReadsPlugin};
use files::{FileTools, FileWriteCoordinator};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{path::PathBuf, sync::Arc};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Host tools this capability provides.
pub const TOOL_NAMES: [&str; 7] = ["read", "write", "edit", "glob", "grep", "exec", "exec_stop"];
const OUTPUT_METHOD: &str = "session.exec.output";
/// `session.exec.output.page`: the tail's sibling, reading the task's
/// registered output file by byte cursor.
const PAGE_OUTPUT_METHOD: &str = "session.exec.output.page";
/// Default page and largest page `session.exec.output.page` serves; a
/// caller's `limit` clamps to the latter, following `session.files.search`'s
/// own clamp.
const OUTPUT_PAGE_BYTES: u64 = 1024 * 1024;
const OUTPUT_PAGE_BYTES_MAX: u64 = 4 * 1024 * 1024;
/// The `session.files` namespace: the files an `@` mention offers.
const FILES_NAMESPACE: &str = "session.files";
const SEARCH_METHOD: &str = "session.files.search";

#[derive(Debug, Clone)]
pub struct WorkspaceToolsConfig {
    /// Absolute root of command output files, `<output_root>/<session_id>/`.
    pub output_root: PathBuf,
    /// The root boundary of a session's file tools, when a host configures
    /// one. The product passes `None`: the tools keep their unbounded
    /// resolution, and ADR 014's facts keep deciding file access.
    pub boundary: Option<files::FileToolBoundary>,
}

/// Re-exported for hosts that configure a boundary: the scenario evaluation
/// harness.
pub use files::FileToolBoundary;

/// Installs WorkspaceTools: the process owner, which registers its Session and
/// branch parts, and the parts themselves. The owner holds the file-write
/// coordinator, one lock domain for every Session it serves, across their
/// generations and its own, and hands it to each Session's part.
pub fn install(builder: &mut AgentBuilder, config: WorkspaceToolsConfig) {
    let coordinator = Arc::<FileWriteCoordinator>::default();
    builder.plugin(move |names| {
        Ok(Box::new(WorkspaceToolsOwner::new(
            names,
            config.clone(),
            coordinator.clone(),
        )?) as Box<dyn DynPlugin>)
    });
    builder.require_part("WorkspaceTools");
    builder.require_part("FileReads");
    builder.require_part("BackgroundExec");
}

/// The process plugin: it registers the Session part and both branch parts
/// with the part registry, holds the file-write coordinator, and provides the
/// one `MentionIndex` its Session parts read, so `session.files.search` and
/// `workspace.files.search` share one walked index (WorkspaceTools §1).
struct WorkspaceToolsOwner {
    parts: Key<PartRegistry>,
    config: WorkspaceToolsConfig,
    coordinator: Arc<FileWriteCoordinator>,
}
impl WorkspaceToolsOwner {
    fn new(
        names: &Names<'_>,
        config: WorkspaceToolsConfig,
        coordinator: Arc<FileWriteCoordinator>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            parts: names.key()?,
            config,
            coordinator,
        })
    }
}
impl DynPlugin for WorkspaceToolsOwner {
    fn name(&self) -> &str {
        "WorkspaceToolsOwner"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let parts = ctx.require_at(&self.parts)?;
            // One index per process, read by every Session part this owner
            // serves and by the App Server's sessionless search.
            ctx.provide_as::<mentions::MentionIndex>(Arc::new(mentions::MentionIndex::default()))?;
            let config = self.config.clone();
            let coordinator = self.coordinator.clone();
            parts.register(ctx, PartScope::Session, "WorkspaceTools", move |mount| {
                Ok(Some(Box::new(WorkspaceToolsPlugin::new(
                    mount.names,
                    config.clone(),
                    coordinator.clone(),
                )?) as Box<dyn DynPlugin>))
            })?;
            parts.register(ctx, PartScope::Branch, "FileReads", |mount| {
                Ok(Some(
                    Box::new(FileReadsPlugin::new(mount.names)?) as Box<dyn DynPlugin>
                ))
            })?;
            parts.register(ctx, PartScope::Branch, "BackgroundExec", |mount| {
                Ok(Some(
                    Box::new(BackgroundExecPlugin::new(mount.names)?) as Box<dyn DynPlugin>
                ))
            })?;
            Ok(Effect::Done)
        })
    }
}

pub(crate) struct WorkspaceTools {
    ctx: Ctx,
    facts: Key<crate::file_access::FileAccessFacts>,
    platform: Arc<HostPlatform>,
    cwd: PathBuf,
    output_dir: PathBuf,
    /// SessionStore's output ownership (ADR 013); absent in hosts without
    /// native storage, where no output has an owner.
    owner: Option<Arc<SessionFilesHost>>,
    session_id: String,
    /// Collaboration members keep commands in the foreground: the leader
    /// treats member idle as the end of a request, so a backgrounded command
    /// would finish after its result. Member asynchronous execution is a
    /// separate design.
    member: bool,
    tree: Arc<TreeCommands>,
    files: FileTools,
    /// Cancelled on unload: new calls settle as reloading, running ones stop.
    lifecycle: CancellationToken,
    /// Calls in flight, including a command being handed to BackgroundExec.
    calls: TaskTracker,
}

fn host_error(error: AgentError) -> HostError {
    match error {
        AgentError::Reloading(_) => HostError::Cancelled(CAPABILITY_RELOADING.into()),
        error => HostError::failed(error.to_string()),
    }
}

impl WorkspaceTools {
    async fn run(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, HostError> {
        let _call = self.calls.token();
        if self.lifecycle.is_cancelled() {
            return Err(HostError::Cancelled(CAPABILITY_RELOADING.into()));
        }
        let branch = self.tree.get(&context.branch_id).map_err(host_error)?;
        let combined = self.lifecycle.child_token();
        let link = async {
            cancel.cancelled().await;
            combined.cancel();
            std::future::pending::<()>().await
        };
        let work = async {
            if call.name == "exec" {
                exec::execute(self, &branch, &call, &context, combined.clone()).await
            } else if call.name == "exec_stop" {
                exec::stop(&branch, &call, &combined).await
            } else if FileTools::handles(&call.name) {
                let reads = branch.service::<FileReads>().map_err(host_error)?;
                self.files.execute(&call, &reads, combined.clone()).await
            } else {
                Ok(ToolResult::error(format!("Unknown tool: {}", call.name)))
            }
        };
        let result = tokio::select! {
            result = work => result,
            _ = link => unreachable!("the link never completes"),
        };
        let result = match result {
            Err(HostError::Cancelled(_)) if self.lifecycle.is_cancelled() => {
                return Err(HostError::Cancelled(CAPABILITY_RELOADING.into()));
            }
            result => result?,
        };
        self.register_output(&result).await?;
        Ok(result)
    }

    /// ADR 013 D2: an output path the result hands to the model is registered
    /// before that result leaves this part. The model's path is the file's
    /// path; a registration failure fails the call.
    async fn register_output(&self, result: &ToolResult) -> Result<(), HostError> {
        let Some(owner) = &self.owner else {
            return Ok(());
        };
        let Some(fields) = &result.structured_content else {
            return Ok(());
        };
        let Some(path) = fields.get("outputPath").and_then(|value| value.as_str()) else {
            return Ok(());
        };
        let path = PathBuf::from(path);
        if !path.starts_with(&self.output_dir) {
            return Err(HostError::failed(format!(
                "Output path {} is outside the Session's output directory",
                path.display()
            )));
        }
        owner
            .0
            .register(self.session_id.clone(), path)
            .await
            .map_err(|failure| HostError::failed(failure.to_string()))
    }
}

impl ToolDriver for WorkspaceTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = FileTools::definitions();
        definitions.push(exec::definition_for(&self.platform));
        definitions.push(exec::stop_definition());
        definitions
    }
    fn resolve_write(
        &self,
        mut call: ToolCall,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolCall> {
        Box::pin(async move {
            if self.lifecycle.is_cancelled() {
                return Err(HostError::Cancelled(CAPABILITY_RELOADING.into()));
            }
            if matches!(call.name.as_str(), "write" | "edit")
                && let Some(path) = call.input.get("path").and_then(|value| value.as_str())
                && !path.is_empty()
            {
                // One effective home: the boundary's when configured, the
                // platform's otherwise — the same home the executed tool
                // resolves `~` against, so pre-admission resolution cannot
                // disagree with execution.
                let (cwd, home, path) = (
                    self.cwd.clone(),
                    self.files.home().to_owned(),
                    path.to_owned(),
                );
                let target = tokio::task::spawn_blocking(move || {
                    files::resolve_path(&cwd, &home, None, files::Access::Write, &path)
                });
                let path = tokio::select! {
                    result = target => result.map_err(|error| HostError::failed(error.to_string()))?
                        .map_err(HostError::failed)?,
                    _ = cancel.cancelled() => return Err(HostError::Cancelled("Tool execution cancelled.".into())),
                };
                let path = path
                    .into_os_string()
                    .into_string()
                    .map_err(|_| HostError::failed("The resolved write path is not UTF-8"))?;
                call.input
                    .insert("path".into(), serde_json::Value::String(path));
            }
            Ok(call)
        })
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(self.run(call, context, cancel))
    }
}

struct WorkspaceToolsPlugin {
    facts: Key<crate::file_access::FileAccessFacts>,
    host: Key<ToolHost>,
    platform: Key<HostPlatform>,
    session_config: Key<SessionConfig>,
    identity: Key<SessionIdentity>,
    tree: Key<TreeCommands>,
    methods: Key<SessionMethods>,
    /// The owner's one walked index, shared with `workspace.files.search`.
    mentions: Key<mentions::MentionIndex>,
    config: WorkspaceToolsConfig,
    coordinator: Arc<FileWriteCoordinator>,
}
impl WorkspaceToolsPlugin {
    fn new(
        names: &Names<'_>,
        config: WorkspaceToolsConfig,
        coordinator: Arc<FileWriteCoordinator>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            facts: names.key()?,
            host: names.key()?,
            platform: names.key()?,
            session_config: names.key()?,
            identity: names.key()?,
            tree: names.key()?,
            methods: names.key()?,
            mentions: names.key()?,
            config,
            coordinator,
        })
    }
}
impl DynPlugin for WorkspaceToolsPlugin {
    fn name(&self) -> &str {
        "WorkspaceTools"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.platform.erase(),
            self.session_config.erase(),
            self.identity.erase(),
            self.tree.erase(),
            self.methods.erase(),
            self.mentions.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            // Fail closed where the handle-based path operations do not
            // exist: a configured boundary would silently degrade to the
            // unbounded path, so the Session part refuses to construct.
            #[cfg(not(unix))]
            if self.config.boundary.is_some() {
                return Err(rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    "the file-tool root boundary needs handle-based path operations, which this platform does not provide".into(),
                )));
            }
            let platform = ctx.require_at(&self.platform)?;
            let config = ctx.require_at(&self.session_config)?;
            let identity = ctx.require_at(&self.identity)?;
            let tree = ctx.require_at(&self.tree)?;
            let methods = ctx.require_at(&self.methods)?;
            let cwd = PathBuf::from(&config.cwd);
            let output_dir = self.config.output_root.join(&identity.session_id);
            // A configured boundary is the file tools' home too: `~`
            // resolves against it, never against the platform home.
            let home = self
                .config
                .boundary
                .as_ref()
                .map_or(platform.home.clone(), |boundary| boundary.home.clone());
            let tools = Arc::new(WorkspaceTools {
                ctx: ctx.clone(),
                facts: self.facts.clone(),
                files: FileTools::with_coordinator(
                    cwd.clone(),
                    home,
                    output_dir.clone(),
                    self.coordinator.clone(),
                    self.config.boundary.clone(),
                ),
                output_dir,
                owner: ctx.get::<SessionFilesHost>(),
                session_id: identity.session_id.clone(),
                member: config.collaboration_role == CollaborationRole::Member,
                cwd,
                platform,
                tree,
                lifecycle: CancellationToken::new(),
                calls: TaskTracker::new(),
            });
            let owner = tools.clone();
            ctx.effect(move || async move {
                owner.lifecycle.cancel();
                owner.calls.close();
                owner.calls.wait().await;
                Ok(())
            })?;
            // The owner's one searched index, shared with
            // `workspace.files.search`.
            let mentions = ctx.require_at(&self.mentions)?;
            let search_root = tools.cwd.clone();
            methods.register(
                ctx,
                FILES_NAMESPACE,
                vec![MethodDecl::new(SEARCH_METHOD, false, Interruption::Report)],
                Arc::new(move |call, _, _| {
                    let MethodCall { params: value, .. } = call;
                    let (mentions, root) = (mentions.clone(), search_root.clone());
                    Box::pin(async move {
                        #[derive(serde::Deserialize)]
                        #[serde(rename_all = "camelCase", deny_unknown_fields)]
                        struct Params {
                            session_id: String,
                            query: String,
                            limit: Option<usize>,
                        }
                        let Params {
                            session_id,
                            query,
                            limit,
                        } = params(value)?;
                        if session_id.is_empty() {
                            return Err(RpcError::invalid("sessionId must not be empty"));
                        }
                        let limit = mentions::parse_limit(limit);
                        let found = tokio::task::spawn_blocking(move || {
                            mentions.search(&root, &query, limit)
                        })
                        .await
                        .map_err(|error| {
                            RpcError::app(-32000, "session_files_failed", error.to_string())
                        })?;
                        reply(serde_json::json!({"candidates": mentions::candidates(&found)}))
                    })
                }),
            )?;
            let pending_tree = tools.tree.clone();
            methods.register(
                ctx,
                "session.exec.pending",
                vec![MethodDecl::new(
                    "session.exec.pending",
                    false,
                    Interruption::Report,
                )],
                Arc::new(move |call, cancel, _| {
                    let tree = pending_tree.clone();
                    Box::pin(async move { pending::read(&tree, call.params, &cancel).await })
                }),
            )?;
            let tree = tools.tree.clone();
            methods.register(
                ctx,
                OUTPUT_METHOD,
                vec![
                    MethodDecl::new(OUTPUT_METHOD, false, Interruption::Report),
                    MethodDecl::new(PAGE_OUTPUT_METHOD, false, Interruption::Report),
                ],
                Arc::new(move |call, _, _| {
                    let MethodCall { method, params: value, .. } = call;
                    let tree = tree.clone();
                    Box::pin(async move {
                        if method == PAGE_OUTPUT_METHOD {
                            #[derive(serde::Deserialize)]
                            #[serde(rename_all = "camelCase", deny_unknown_fields)]
                            struct PageParams {
                                session_id: String,
                                branch_id: String,
                                task_id: String,
                                cursor: Option<u64>,
                                limit: Option<u64>,
                            }
                            let PageParams {
                                session_id,
                                branch_id,
                                task_id,
                                cursor,
                                limit,
                            } = params(value)?;
                            if session_id.is_empty() || branch_id.is_empty() || task_id.is_empty() {
                                return Err(RpcError::invalid(
                                    "sessionId, branchId and taskId must not be empty",
                                ));
                            }
                            let limit = match limit {
                                Some(0) => {
                                    return Err(RpcError::invalid("limit must be at least 1"))
                                }
                                Some(limit) => limit.clamp(1, OUTPUT_PAGE_BYTES_MAX),
                                None => OUTPUT_PAGE_BYTES,
                            };
                            let page = tree
                                .get(&branch_id)?
                                .service::<background::BackgroundExec>()?
                                .output_page(&task_id, cursor.unwrap_or(0), limit as usize)
                                .await
                                .map_err(|error| match error {
                                    background::OutputPageError::Unknown => RpcError::app(
                                        -32000,
                                        "exec_task_unknown",
                                        "No such task is known on this branch",
                                    ),
                                    background::OutputPageError::Unavailable => RpcError::app(
                                        -32000,
                                        "exec_output_unavailable",
                                        "The task's output file is no longer available; it was removed or its Session was deleted",
                                    ),
                                    background::OutputPageError::Closed => RpcError::reloading(),
                                    background::OutputPageError::Read(error) => error.into(),
                                })?;
                            return reply(page);
                        }
                        #[derive(serde::Deserialize)]
                        #[serde(rename_all = "camelCase", deny_unknown_fields)]
                        struct Params {
                            session_id: String,
                            branch_id: String,
                            task_id: String,
                        }
                        let Params {
                            session_id,
                            branch_id,
                            task_id,
                        } = params(value)?;
                        if session_id.is_empty() || branch_id.is_empty() || task_id.is_empty() {
                            return Err(RpcError::invalid(
                                "sessionId, branchId and taskId must not be empty",
                            ));
                        }
                        let text = tree
                            .get(&branch_id)?
                            .service::<background::BackgroundExec>()?
                            .output(&task_id)
                            .await
                            .map_err(|error| match error {
                                background::TaskError::Unknown => RpcError::app(
                                    -32000,
                                    "exec_task_unknown",
                                    "No live output is available for this task",
                                ),
                                background::TaskError::Closed => RpcError::reloading(),
                                background::TaskError::Read(error) => error.into(),
                            })?;
                        Ok(serde_json::json!({"text":text}))
                    })
                }),
            )?;
            ctx.provide_at(self.host.clone(), Arc::new(ToolHost(tools)))?;
            Ok(Effect::Done)
        })
    }
}

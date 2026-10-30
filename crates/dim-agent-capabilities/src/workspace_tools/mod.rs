//! Workspace tools, see `docs/architecture/rust-agent/plugins/WorkspaceTools.md`.
//!
//! `WorkspaceToolsPlugin` (Session) provides `S:ToolHost` executing `read`
//! `write` `edit` `glob` `grep` `exec` `exec_stop`. Branch parts are looked up per call on
//! the calling branch: `FileReads` for file tools and `BackgroundExec` for a
//! command that moves to the background. Admission happened before a call
//! reaches execution; `resolve_write` only prepares its target before admission.
//! Nothing here decides permissions, approvals or sandboxes.
pub mod background;
pub mod exec;
pub mod file_reads;
pub mod files;

use crate::{
    host::{HostError, HostFuture, ToolDriver, ToolExecutionContext, ToolHost},
    host_platform::HostPlatform,
    methods::{Interruption, MethodDecl, RpcError, SessionMethods, params},
    timeline_tree::TreeCommands,
};
use background::BackgroundExecPlugin;
use dim_agent_runtime::{
    AgentBuilder, AgentError, CAPABILITY_RELOADING, CollaborationRole, Names, SessionConfig,
    SessionIdentity,
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

#[derive(Debug, Clone)]
pub struct WorkspaceToolsConfig {
    /// Absolute root of command output files, `<output_root>/<session_id>/`.
    pub output_root: PathBuf,
}

/// Installs the WorkspaceTools plugin: its Session part and its branch
/// parts. The plugin holds the file-write coordinator, one lock domain for
/// every Session it serves, across their generations and its own, and hands
/// it to each Session's part.
pub fn install(builder: &mut AgentBuilder, config: WorkspaceToolsConfig) {
    let coordinator = Arc::<FileWriteCoordinator>::default();
    builder.require_feature(
        dim_agent_runtime::Feature::new("WorkspaceTools")
            .session("WorkspaceTools", move |names| {
                Ok(Box::new(WorkspaceToolsPlugin::new(
                    names,
                    config.clone(),
                    coordinator.clone(),
                )?))
            })
            .branch("FileReads", |names| {
                Ok(Box::new(FileReadsPlugin::new(names)?))
            })
            .branch("BackgroundExec", |names| {
                Ok(Box::new(BackgroundExecPlugin::new(names)?))
            }),
    );
}

pub(crate) struct WorkspaceTools {
    platform: Arc<HostPlatform>,
    cwd: PathBuf,
    output_dir: PathBuf,
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
        match result {
            Err(HostError::Cancelled(_)) if self.lifecycle.is_cancelled() => {
                Err(HostError::Cancelled(CAPABILITY_RELOADING.into()))
            }
            result => result,
        }
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
                let (cwd, home, path) = (
                    self.cwd.clone(),
                    self.platform.home.clone(),
                    path.to_owned(),
                );
                let target =
                    tokio::task::spawn_blocking(move || files::resolve_path(&cwd, &home, &path));
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
    host: Key<ToolHost>,
    platform: Key<HostPlatform>,
    session_config: Key<SessionConfig>,
    identity: Key<SessionIdentity>,
    tree: Key<TreeCommands>,
    methods: Key<SessionMethods>,
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
            host: names.key()?,
            platform: names.key()?,
            session_config: names.key()?,
            identity: names.key()?,
            tree: names.key()?,
            methods: names.key()?,
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
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let platform = ctx.require_at(&self.platform)?;
            let config = ctx.require_at(&self.session_config)?;
            let identity = ctx.require_at(&self.identity)?;
            let tree = ctx.require_at(&self.tree)?;
            let methods = ctx.require_at(&self.methods)?;
            let cwd = PathBuf::from(&config.cwd);
            let output_dir = self.config.output_root.join(&identity.session_id);
            let tools = Arc::new(WorkspaceTools {
                files: FileTools::with_coordinator(
                    cwd.clone(),
                    platform.home.clone(),
                    output_dir.clone(),
                    self.coordinator.clone(),
                ),
                output_dir,
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
            let tree = tools.tree.clone();
            methods.register(
                ctx,
                OUTPUT_METHOD,
                vec![MethodDecl::new(OUTPUT_METHOD, false, Interruption::Report)],
                Arc::new(move |_, value, _, _| {
                    let tree = tree.clone();
                    Box::pin(async move {
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

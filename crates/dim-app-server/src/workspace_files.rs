//! The sessionless workspace file search, `workspace.files.search`
//! (WorkspaceTools §1). The row injects the process `MentionIndex` the
//! WorkspaceTools owner provides, so the Session part that serves
//! `session.files.search` reads the same walked index. It is assembled only
//! where the profile requires the WorkspaceTools part. A local connection is
//! the user: an absolute, canonical, readable directory is the whole
//! authorization, as `skills.catalog` decides, and nothing is registered with
//! Settings.
use crate::methods::ManagementMethods;
use dim_agent_capabilities::{
    methods::{Interruption, MethodCall, MethodDecl, RpcError, params, reply},
    workspace_tools::mentions::{self, MentionIndex},
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// The namespace and its one method; management namespaces may not lie under
/// the Session roots (method contract A.1).
const NAMESPACE: &str = "workspace.files";
const SEARCH_METHOD: &str = "workspace.files.search";

pub(crate) struct WorkspaceFilesPlugin {
    methods: Key<ManagementMethods>,
    index: Key<MentionIndex>,
}

impl WorkspaceFilesPlugin {
    pub(crate) fn new(names: &dim_agent_runtime::Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            methods: names.key()?,
            index: names.key()?,
        })
    }
}

impl DynPlugin for WorkspaceFilesPlugin {
    fn name(&self) -> &str {
        "WorkspaceFiles"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.methods.erase(), self.index.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            // The owner's one index: the WorkspaceTools Session part reads the
            // same service, so both entry points share one walk.
            let index = ctx.require_at(&self.index)?;
            methods.register(
                ctx,
                NAMESPACE,
                vec![MethodDecl::new(SEARCH_METHOD, false, Interruption::Report)],
                Arc::new(move |call, _, _| {
                    let MethodCall { params: value, .. } = call;
                    let index = index.clone();
                    Box::pin(async move {
                        #[derive(serde::Deserialize)]
                        #[serde(deny_unknown_fields)]
                        struct Params {
                            cwd: String,
                            query: String,
                            limit: Option<usize>,
                        }
                        let Params { cwd, query, limit } = params(value)?;
                        let limit = mentions::parse_limit(limit);
                        let root = tokio::task::spawn_blocking(move || canonical_cwd(&cwd))
                            .await
                            .map_err(|error| {
                                RpcError::app(-32000, "session_files_failed", error.to_string())
                            })??;
                        let found =
                            tokio::task::spawn_blocking(move || index.search(&root, &query, limit))
                                .await
                                .map_err(|error| {
                                    RpcError::app(-32000, "session_files_failed", error.to_string())
                                })?;
                        reply(json!({"candidates": mentions::candidates(&found)}))
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

/// `cwd` as the search root: absolute, canonical and an existing directory.
fn canonical_cwd(cwd: &str) -> Result<PathBuf, RpcError> {
    let path = PathBuf::from(cwd);
    if !path.is_absolute() {
        return Err(RpcError::invalid("cwd must be an absolute path"));
    }
    let root = path
        .canonicalize()
        .map_err(|_| RpcError::invalid("cwd must name an existing directory"))?;
    if !root.is_dir() {
        return Err(RpcError::invalid("cwd must name a directory"));
    }
    Ok(root)
}

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

mod completion;

const NAMESPACE: &str = "workspace.files";
const DIRECTORIES: &str = "workspace.files.directories";
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
                vec![
                    MethodDecl::new(SEARCH_METHOD, false, Interruption::Report),
                    MethodDecl::new("workspace.files.complete", false, Interruption::Report),
                    MethodDecl::new(DIRECTORIES, false, Interruption::Report),
                ],
                Arc::new(move |call, _, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let index = index.clone();
                    Box::pin(async move {
                        if method == "workspace.files.complete" {
                            let request = params(value)?;
                            return reply(
                                tokio::task::spawn_blocking(move || {
                                    completion::complete(&index, request)
                                })
                                .await
                                .map_err(|e| {
                                    RpcError::service("session_files_failed", e.to_string())
                                })??,
                            );
                        }
                        if method == DIRECTORIES {
                            let DirectoryRequest { path } = params(value)?;
                            let page = tokio::task::spawn_blocking(move || directories(path))
                                .await
                                .map_err(|e| {
                                    RpcError::service("directory_failed", e.to_string())
                                })??;
                            return reply(page);
                        }
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryRequest {
    path: Option<String>,
}

#[derive(serde::Serialize)]
struct DirectoryPage {
    path: Option<String>,
    parent: Option<String>,
    directories: Vec<Directory>,
}

#[derive(serde::Serialize)]
struct Directory {
    name: String,
    path: String,
}

fn directories(path: Option<String>) -> Result<DirectoryPage, RpcError> {
    #[cfg(windows)]
    if path.as_deref().unwrap_or_default().is_empty() {
        let directories = ('A'..='Z')
            .map(|drive| format!("{drive}:\\"))
            .filter(|root| std::path::Path::new(root).is_dir())
            .map(|path| Directory {
                name: path.clone(),
                path,
            })
            .collect();
        return Ok(DirectoryPage {
            path: None,
            parent: None,
            directories,
        });
    }
    let dir = canonical_cwd(path.as_deref().unwrap_or("/"))?;
    let fail = |err: std::io::Error| RpcError::service("directory_failed", err.to_string());
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(fail)? {
        let entry = entry.map_err(fail)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        #[cfg(windows)]
        if std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 2 != 0 {
            continue;
        }
        #[cfg(target_os = "macos")]
        if std::os::darwin::fs::MetadataExt::st_flags(&metadata) & libc::UF_HIDDEN != 0 {
            continue;
        }
        if let Some(path) = entry.path().to_str() {
            directories.push(Directory {
                name: name.to_owned(),
                path: path.to_owned(),
            });
        }
    }
    directories.sort_by(|a, b| a.name.cmp(&b.name));
    let parent = dir
        .parent()
        .and_then(|p| p.to_str())
        .map(String::from)
        .or_else(|| cfg!(windows).then(String::new));
    Ok(DirectoryPage {
        path: Some(
            dir.into_os_string()
                .into_string()
                .map_err(|_| RpcError::invalid("directory path requires UTF-8"))?,
        ),
        parent,
        directories,
    })
}

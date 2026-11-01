//! The fixed file-access facts `Permission` decides a file call from
//! ([ADR 014 D1](../../../docs/adr/014-file-access-facts.md#d1-fixed-facts)).
//! The service reads Session configuration, the Session's owned artifacts and
//! its grants at use time and holds no decision state.
use crate::{
    host::{HostError, HostFuture, SessionFilesHost},
    host_platform::HostPlatform,
    session_grants::SessionGrantsHost,
    workspace_tools::files::canonical_prefix,
};
use dim_agent_runtime::{AgentBuilder, Feature, Names, SessionConfig};
use dim_agent_types::{FileAccess, grant::GrantMode};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

/// The fixed facts for one resolved path under one branch preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFacts {
    /// The path is inside the Session workspace.
    pub workspace: bool,
    /// The path is inside one of HostPlatform's fixed system roots.
    pub system: bool,
    /// The path is a Session-owned artifact.
    pub owned: bool,
    /// A Session grant permits `mode` under `preset`; a write grant is inert
    /// under `read-only` and a read grant never permits a write.
    pub grant: bool,
}

/// The `fileAccessFacts` service. `path` is already resolved, so `Permission`
/// compares the same canonical path it admits.
pub trait ReadFileAccessFacts: Send + Sync + 'static {
    fn facts(
        &self,
        session_id: &str,
        access: FileAccess,
        path: &Path,
        mode: GrantMode,
    ) -> HostFuture<'_, FileFacts>;
}

/// The Session service `fileAccessFacts`, provided where a Session has
/// SessionStore's `sessionFiles` and its grants.
pub struct FileAccessFacts(pub Arc<dyn ReadFileAccessFacts>);

/// Installs the facts service. A profile calls this where `sessionFiles` and
/// `SessionGrantsHost` are bound.
pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(
        Feature::new("FileAccessFacts").session("FileAccessFacts", |names| {
            Ok(Box::new(FileAccessFactsPlugin::new(names)?))
        }),
    );
}

struct Facts {
    config: Arc<SessionConfig>,
    platform: Arc<HostPlatform>,
    files: Arc<SessionFilesHost>,
    grants: Arc<SessionGrantsHost>,
}

impl ReadFileAccessFacts for Facts {
    fn facts(
        &self,
        session_id: &str,
        access: FileAccess,
        path: &Path,
        mode: GrantMode,
    ) -> HostFuture<'_, FileFacts> {
        let path = path.to_path_buf();
        let session_id = session_id.to_owned();
        Box::pin(async move {
            let cwd = PathBuf::from(&self.config.cwd);
            let workspace = tokio::task::spawn_blocking(move || canonical_prefix(cwd))
                .await
                .map_err(|error| {
                    HostError::failed(format!("file access facts task failed: {error}"))
                })?
                .map_err(HostError::failed)?;
            let owned = self
                .files
                .0
                .owns(session_id, path.clone())
                .await
                .map_err(|error| HostError::failed(error.to_string()))?;
            let grants = self.grants.0.grants(CancellationToken::new()).await?;
            let grant = grants.iter().any(|grant| match mode {
                GrantMode::Write => {
                    access == FileAccess::WorkspaceReadWrite && grant.matches(&path, mode)
                }
                GrantMode::Read => grant.matches(&path, mode),
            });
            Ok(FileFacts {
                workspace: path.starts_with(&workspace),
                system: self
                    .platform
                    .platform_roots
                    .iter()
                    .any(|root| path.starts_with(root)),
                owned,
                grant,
            })
        })
    }
}

struct FileAccessFactsPlugin {
    config: Key<SessionConfig>,
    platform: Key<HostPlatform>,
    files: Key<SessionFilesHost>,
    grants: Key<SessionGrantsHost>,
    service: Key<FileAccessFacts>,
}
impl FileAccessFactsPlugin {
    fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            config: names.key()?,
            platform: names.key()?,
            files: names.key()?,
            grants: names.key()?,
            service: names.key()?,
        })
    }
}
impl DynPlugin for FileAccessFactsPlugin {
    fn name(&self) -> &str {
        "FileAccessFacts"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.config.erase(),
            self.platform.erase(),
            self.files.erase(),
            self.grants.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let service = Arc::new(FileAccessFacts(Arc::new(Facts {
                config: ctx.require_at(&self.config)?,
                platform: ctx.require_at(&self.platform)?,
                files: ctx.require_at(&self.files)?,
                grants: ctx.require_at(&self.grants)?,
            })));
            ctx.provide_at(self.service.clone(), service)?;
            Ok(Effect::Done)
        })
    }
}

//! The Session port of the hook documents (Hooks A.2, appendix H): the
//! agent-side parts never name Settings or App Server.
use super::document::HooksDocument;
use crate::host::{HostError, HostFuture};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

/// One source of hook documents, in catalog order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookSourceId {
    User,
    Project,
    /// The document a plugin registered, named by its plugin id (A.2).
    Plugin(String),
}
/// The source's id in the catalog and in the `dim_source` field: `user`,
/// `project` or `plugin:<id>` (A.2, A.3).
impl fmt::Display for HookSourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Project => f.write_str("project"),
            Self::Plugin(plugin) => write!(f, "plugin:{plugin}"),
        }
    }
}
impl serde::Serialize for HookSourceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Whether a source's document was read, absent or refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookSourceState {
    Ok,
    Missing,
    Invalid(String),
}

/// One source as the port returns it.
#[derive(Clone, Debug)]
pub struct HookSourceSnapshot {
    pub id: HookSourceId,
    /// Where the document lives: the user file, the project file, or a
    /// plugin's directory, which its commands read as `DIM_PLUGIN_ROOT` (A.2,
    /// A.3).
    pub path: Option<PathBuf>,
    pub enabled: bool,
    pub state: HookSourceState,
    /// Parsed only when the state is `Ok`.
    pub document: Option<HooksDocument>,
}

/// Every effective source of one working directory, in catalog order.
#[derive(Clone, Debug, Default)]
pub struct HookSourcesSnapshot {
    pub sources: Vec<HookSourceSnapshot>,
}

/// The hook documents for a working directory. `cwd` resolves the project
/// source the same way the configuration owner authorizes it.
pub trait ReadHookSources: Send + Sync + 'static {
    /// Errors mean no source could be read; the hook point then runs nothing.
    fn sources(&self, cwd: &Path, cancel: CancellationToken)
    -> HostFuture<'_, HookSourcesSnapshot>;
    /// The product home of the App Server that runs the Session, which a hook
    /// reads as `DIMCODE_HOME`; `None` when the App Server has none.
    fn home(&self) -> Option<PathBuf>;
}

/// Session port of the hook sources; each profile binds one.
pub struct HookSourcesHost(pub Arc<dyn ReadHookSources>);
impl std::fmt::Debug for HookSourcesHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookSourcesHost").finish_non_exhaustive()
    }
}

/// The sources of a host that keeps no hook documents.
pub struct NoHookSources;
impl ReadHookSources for NoHookSources {
    fn sources(&self, _: &Path, _: CancellationToken) -> HostFuture<'_, HookSourcesSnapshot> {
        Box::pin(async { Ok(HookSourcesSnapshot::default()) })
    }
    fn home(&self) -> Option<PathBuf> {
        None
    }
}

impl HookSourcesHost {
    pub async fn read(
        &self,
        cwd: &Path,
        cancel: &CancellationToken,
    ) -> Result<HookSourcesSnapshot, HostError> {
        self.0.sources(cwd, cancel.clone()).await
    }
}

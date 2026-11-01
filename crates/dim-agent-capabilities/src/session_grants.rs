//! The Session's file grants, read when Permission is about to ask.
use crate::host::HostFuture;
use dim_agent_types::grant::SessionGrant;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub trait ReadSessionGrants: Send + Sync + 'static {
    fn grants(&self, cancel: CancellationToken) -> HostFuture<'_, Vec<SessionGrant>>;
}

pub struct SessionGrantsHost(pub Arc<dyn ReadSessionGrants>);

/// Resolves a user- or model-supplied grant path against the Session
/// workspace: expands `~`, joins a relative path, folds `.` and `..`, and
/// takes the realpath of the longest existing prefix, so a file that does not
/// exist yet is resolved by its parent and final name.
pub fn resolve_grant_path(cwd: &Path, path: &str) -> Result<PathBuf, String> {
    let home = if path.starts_with('~') {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("Cannot expand {}: no home directory", path))?
    } else {
        PathBuf::new()
    };
    crate::workspace_tools::files::resolve_path(cwd, &home, path)
}

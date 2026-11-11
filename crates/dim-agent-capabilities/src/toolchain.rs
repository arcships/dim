//! The runtimes RuntimeManager downloaded, as HostPlatform reads them at each
//! process start. RuntimeManager owns the runtimes and their record; this port
//! carries only the toolchains installed under the plan in effect, which
//! HostPlatform appends to the PATH of the processes it starts and grants to a
//! confined one read-only. A host without it starts every process with the
//! PATH it would have without any runtime.
use std::path::PathBuf;
use std::sync::Arc;

/// One runtime or tool Dim downloaded and installed (§2.4). It is the
/// runtimes and the uv tool RuntimeManager owns that a started process may
/// run; a `system` interpreter is the user's own and is reached through the
/// user's `PATH`, so it is never one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledToolchain {
    /// The directory holding the interpreter: appended to a child's PATH.
    pub bin: PathBuf,
    /// The runtime's archive directory, which contains `bin`: a read-only
    /// root of a confined command.
    pub root: PathBuf,
}

/// One installed runtime in the `list` reply. The two directories are UTF-8
/// paths, as every path that crosses is
/// ([service faces §1](../../../docs/design/service-faces.md#1-规则)).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolchainView {
    pub bin: String,
    pub root: String,
}

/// The `list` reply of `toolchainSourceHost`: the installed runtimes, in the
/// fixed order node, python, then uv.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolchainSourceReply {
    pub toolchains: Vec<ToolchainView>,
}

/// What `toolchainSourceHost` fails with: `{code, message}`.
#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ToolchainSourceError {
    #[error("{message}")]
    Failed { message: String },
}

/// An owner's list of the installed runtimes, bound as the face's one
/// operation. Read when a process starts, so a runtime installed meanwhile
/// reaches the next process without a reload.
pub type InstalledToolchains =
    Arc<dyn Fn() -> Result<Vec<InstalledToolchain>, ToolchainSourceError> + Send + Sync>;

/// The `toolchainSourceHost` face: its owner provides it with the runtimes,
/// another language calls `list` by name, and a Rust reader calls the same
/// method directly.
#[derive(Clone)]
pub struct ToolchainSourceHost(InstalledToolchains);

impl ToolchainSourceHost {
    /// Binds an owner's list as the face's operation.
    pub fn new(toolchains: InstalledToolchains) -> Self {
        Self(toolchains)
    }

    /// Toolchains that never change, for an assembly that installs no
    /// runtimes, such as a test or an evaluation.
    pub fn fixed(toolchains: Vec<InstalledToolchain>) -> Self {
        Self::new(Arc::new(move || Ok(toolchains.clone())))
    }
}

#[rutis::service]
impl ToolchainSourceHost {
    /// The installed runtimes, in order.
    pub fn list(&self) -> std::result::Result<ToolchainSourceReply, ToolchainSourceError> {
        let toolchains = (self.0)()?
            .iter()
            .map(view)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|path| ToolchainSourceError::Failed {
                message: format!("toolchain path is not UTF-8: {}", path.display()),
            })?;
        Ok(ToolchainSourceReply { toolchains })
    }
}

fn view(toolchain: &InstalledToolchain) -> Result<ToolchainView, PathBuf> {
    fn text(path: &std::path::Path) -> Result<String, PathBuf> {
        path.to_str().map(str::to_owned).ok_or_else(|| path.into())
    }
    Ok(ToolchainView {
        bin: text(&toolchain.bin)?,
        root: text(&toolchain.root)?,
    })
}

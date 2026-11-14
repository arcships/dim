//! The runtimes RuntimeManager downloaded, as HostPlatform reads them at each
//! process start. RuntimeManager owns the runtimes and their record; this port
//! carries only the toolchains installed under the plan in effect, which
//! HostPlatform appends to the PATH of the processes it starts and grants to a
//! confined one read-only. A host without it starts every process with the
//! PATH it would have without any runtime.
use crate::port_faces::{self, Empty};
use rutis::Ctx;
use rutis::host::{CtxHost, Key};
use rutis_bridge::session::{HostDispatch, Reply, Value as BridgeValue};
use serde_json::json;
use std::path::{Path, PathBuf};
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
/// fixed order node, python, then uv. An entry whose directory is not a UTF-8
/// path cannot cross: it is left out, so the others still reach a child.
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

/// An owner's list of the installed runtimes, as the face's one operation.
/// Read when a process starts, so a runtime installed meanwhile reaches the
/// next process without a reload.
pub type InstalledToolchains = Arc<dyn Fn() -> Vec<InstalledToolchain> + Send + Sync>;

/// The `toolchainSourceHost` face: `list` is the one operation, served under
/// the name for any language and called directly by a Rust reader through the
/// binding that holds it.
#[derive(Clone)]
pub struct ToolchainSourceHost(Arc<dyn HostDispatch>);

impl ToolchainSourceHost {
    /// Binds an owner's list as the face's operation.
    pub fn new(toolchains: InstalledToolchains) -> Self {
        Self(Arc::new(Face(toolchains)))
    }

    /// Toolchains that never change, for an assembly that installs no
    /// runtimes, such as a test or an evaluation.
    pub fn fixed(toolchains: Vec<InstalledToolchain>) -> Self {
        Self::new(Arc::new(move || toolchains.clone()))
    }

    /// The installed runtimes, in order.
    pub fn list(&self) -> std::result::Result<ToolchainSourceReply, ToolchainSourceError> {
        let failed = |error: String| ToolchainSourceError::Failed { message: error };
        let request =
            rutis_bridge::session::arg(&Empty {}).map_err(|error| failed(error.to_string()))?;
        let reply = self
            .0
            .invoke("list", BridgeValue::List(vec![request]))
            .map_err(|error| failed(error.to_string()))?;
        let value = reply.json().map_err(|error| failed(error.to_string()))?;
        serde_json::from_value(value).map_err(|error| failed(error.to_string()))
    }
}

/// Provides one owner's [`ToolchainSourceHost`]: the binding its Rust readers
/// hold and the face any other language calls by name.
pub fn provide(ctx: &Ctx, host: &ToolchainSourceHost) -> rutis::Result<()> {
    ctx.provide_at(
        Key::<ToolchainSourceHost>::of(),
        Arc::new(ToolchainSourceHost(host.0.clone())),
    )?;
    ctx.provide_at(
        port_faces::face_key::<ToolchainSourceHost>(),
        host.0.clone(),
    )?;
    Ok(())
}

const NAME: &str = "toolchainSourceHost";

struct Face(InstalledToolchains);

impl HostDispatch for Face {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"list": "sync"}))
    }

    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        if method != "list" {
            return Err(port_faces::unknown_method(NAME, method));
        }
        let Empty {} = port_faces::request(NAME, args)?;
        let toolchains = (self.0)().iter().filter_map(view).collect();
        rutis_bridge::session::arg(&ToolchainSourceReply { toolchains })
    }
}

fn view(toolchain: &InstalledToolchain) -> Option<ToolchainView> {
    fn text(path: &Path) -> Option<String> {
        path.to_str().map(str::to_owned)
    }
    let bin = text(&toolchain.bin)?;
    let root = text(&toolchain.root)?;
    Some(ToolchainView { bin, root })
}

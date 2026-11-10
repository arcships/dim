//! The runtimes RuntimeManager downloaded, as HostPlatform reads them at each
//! process start. RuntimeManager owns the runtimes and their record; this port
//! carries only the toolchains installed under the plan in effect, which
//! HostPlatform appends to the PATH of the processes it starts and grants to a
//! confined one read-only. A host without it starts every process with the
//! PATH it would have without any runtime.
use crate::port_faces::{no_args, port_failure, unknown_method};
use rutis::Ctx;
use rutis::host::{CtxHost, Key};
use rutis_bridge::session::{HostDispatch, Reply, Value as BridgeValue, host_key};
use serde_json::json;
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

/// The downloaded runtimes and tools installed under the plan in effect, in
/// the fixed order node, python, then uv. Read when a process starts, so a
/// runtime installed meanwhile reaches the next process without a reload.
pub trait ToolchainSource: Send + Sync + 'static {
    fn toolchains(&self) -> Vec<InstalledToolchain>;
}

pub struct ToolchainSourceHost(pub Arc<dyn ToolchainSource>);

impl ToolchainSourceHost {
    /// Toolchains that never change, for an assembly that installs no
    /// runtimes, such as a test or an evaluation.
    pub fn fixed(toolchains: Vec<InstalledToolchain>) -> Self {
        struct Fixed(Vec<InstalledToolchain>);
        impl ToolchainSource for Fixed {
            fn toolchains(&self) -> Vec<InstalledToolchain> {
                self.0.clone()
            }
        }
        Self(Arc::new(Fixed(toolchains)))
    }
}

/// Provides one owner's [`ToolchainSourceHost`]: the directories the
/// `HostPlatform` reads, and the `toolchainSourceHost` face other languages
/// read them by.
pub fn provide(ctx: &Ctx, host: &ToolchainSourceHost) -> rutis::Result<()> {
    ctx.provide_at(
        Key::<ToolchainSourceHost>::of(),
        Arc::new(ToolchainSourceHost(host.0.clone())),
    )?;
    ctx.provide_at(
        host_key(&dim_agent_runtime::names::name_of::<ToolchainSourceHost>()),
        Arc::new(ToolchainSourceFace(host.0.clone())) as Arc<dyn HostDispatch>,
    )?;
    Ok(())
}

/// One installed runtime on the wire. The two directories are UTF-8 paths, as
/// every path that crosses is
/// ([service faces §1](../../../docs/design/service-faces.md#1-规则)).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolchainView {
    pub bin: String,
    pub root: String,
}

/// The `toolchainSourceHost.list` reply: the installed runtimes, in order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolchainSourceReply {
    pub toolchains: Vec<ToolchainView>,
}

const NAME: &str = "toolchainSourceHost";

struct ToolchainSourceFace(Arc<dyn ToolchainSource>);

impl HostDispatch for ToolchainSourceFace {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"list": "sync"}))
    }

    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        if method != "list" {
            return Err(unknown_method(NAME, method));
        }
        no_args(NAME, method, args)?;
        let toolchains = self
            .0
            .toolchains()
            .iter()
            .map(view)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|path| {
                port_failure(
                    NAME,
                    "failed",
                    format!("toolchain path is not UTF-8: {}", path.display()),
                )
            })?;
        rutis_bridge::session::arg(&ToolchainSourceReply { toolchains })
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

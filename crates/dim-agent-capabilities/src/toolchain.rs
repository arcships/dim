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

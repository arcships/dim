mod branch;
mod config;
mod host;
mod interpreter;
mod process;
mod runs;
mod session;

#[cfg(test)]
mod tests;

pub use config::CodeModeConfig;
pub use host::NestedToolHost;
pub use runs::{CodeModeRunner, CodeModeRuns};

use dim_agent_runtime::{AgentBuilder, names::register};

pub const CODEMODE_TOOL: &str = "codemode";

/// Adds the process row and its parts as optional parts. A host that wants
/// the `codemode` tool to gate Session publication uses [`install_required`].
pub fn install(builder: &mut AgentBuilder) {
    register::<CodeModeRuns>(builder.catalog_mut(), None);
    builder.plugin(|names| Ok(Box::new(process::CodeModeProcessPlugin::new(names)?)));
}

pub fn install_required(builder: &mut AgentBuilder) {
    install(builder);
    builder
        .require_part("CodeModeSession")
        .require_part("CodeModeBranch");
}

//! Native aimux model hosts for the Agent runtime.
mod auxiliary;
pub mod config;
mod driver;
mod models;
mod plugin;
pub mod recording;
mod request;
mod stream;

pub use models::AimuxModels;
pub use plugin::{AimuxPlugin, ConfiguredModelPlugin, ConfiguredModelsPlugin};

/// The transport a media call uses, with the connection facts and redirect
/// policy a chat call to the same model gets.
pub use driver::MediaTransport;

use aimux_core::AiMuxError;
use dim_agent_capabilities::host::HostError;
use tokio_util::sync::CancellationToken;

/// One aimux failure as the host reports it: the classification and the
/// one-line reason a chat call's failure carries. A consumer that stores the
/// text scrubs it of anything its record may not hold.
pub fn failure(error: AiMuxError, cancel: &CancellationToken) -> HostError {
    stream::error(error, cancel)
}

use dim_agent_runtime::AgentBuilder;

/// Registers the names of this crate's services (plugin contract §4).
pub fn register_names(builder: &mut AgentBuilder) {
    dim_agent_runtime::names::register::<AimuxModels>(builder.catalog_mut(), None);
}

/// Adds ConfiguredModel to every Session: it prepares requests for the
/// models of the catalog ConfiguredModels provides at the process.
pub fn install(builder: &mut AgentBuilder) {
    register_names(builder);
    builder.require_feature(
        dim_agent_runtime::Feature::new("ConfiguredModel").session("ConfiguredModel", |names| {
            Ok(Box::new(ConfiguredModelPlugin::new(names)?))
        }),
    );
}

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

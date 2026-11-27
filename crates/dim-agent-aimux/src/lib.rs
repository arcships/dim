//! Native aimux model hosts for the Agent runtime.
mod auxiliary;
pub mod config;
mod driver;
mod identity;
mod models;
mod plugin;
pub mod recording;
mod request;
mod stream;

pub use models::AimuxModels;
pub use plugin::{AimuxPlugin, ConfiguredModelPlugin, ConfiguredModelsPlugin};

/// The identity one Aimux part puts on its model requests. The host process
/// passes `DIM_USER_AGENT`'s value to [`ModelIdentity::new`]; the App Server's
/// `provider` settings source writes the configured override into the same
/// value.
pub use identity::{ModelIdentity, environment_user_agent};

/// The variable a host process reads once and passes to its model hosts.
pub use identity::ENVIRONMENT_VARIABLE;

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

/// The reasoning efforts this crate's request preparation accepts for the
/// model, in the order the model's own table declares them, or `None` when
/// the table does not constrain the model. This is the per-model table the
/// request path applies to the value it sends (#2189): an effort the table
/// does not name is dropped with a warning that fails the turn, so a catalog
/// serving choices for these protocols intersects with this table rather
/// than offering a value the wire cannot take.
pub fn accepted_reasoning_efforts(model_id: &str) -> Option<&'static [&'static str]> {
    aimux_providers::openai::language_model_capabilities::get_model_capabilities(model_id)
        .supported_reasoning_efforts
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

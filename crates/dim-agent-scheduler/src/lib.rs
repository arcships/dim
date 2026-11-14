//! Scheduled jobs for Dim, implemented natively. The design is
//! [docs/design/plugins/Scheduler.md](../../../docs/design/plugins/Scheduler.md).
//!
//! The process owner keeps jobs in the `schedule` settings document and run
//! history in `scheduler/runs.json`, and serves the `schedule` management
//! namespace.
mod clock;
mod document;
mod engine;
mod loops;
mod methods;
mod runs;
mod session;
mod wakeups;
mod watch;

#[cfg(feature = "testing")]
pub mod testing;

#[cfg(test)]
extern crate self as dim_agent_scheduler;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;
#[cfg(test)]
#[path = "../tests/support/wakeup_pass.rs"]
mod wakeup_pass;

use dim_agent_runtime::{AgentBuilder, Names};
use rutis::host::DynPlugin;

pub use document::ScheduleDocument;
pub use loops::{LOOPS_FILE, Loop, LoopState, Loops};
pub use methods::{ScheduleJobs, SchedulerPlugin};
pub use runs::{RUNS_FILE, SchedulerError};
pub use session::SchedulerSession;
pub use wakeups::Wakeups;

/// Installs the scheduler as an optional feature: the process owner and the
/// `schedule` tool. Neither is required, so a host without the App Server
/// services it needs (settings, pluginFiles, managementMethods and sessions),
/// or with an unreadable run store, still starts and opens Sessions; only
/// `schedule.*` and the tool are unavailable.
pub fn install(builder: &mut AgentBuilder) {
    install_with(builder, SchedulerTexts::default()).expect("the built-in texts are valid");
}

/// The policy texts a profile may replace (design §5.1): the `schedule`
/// declaration's description.
#[derive(Clone, Debug)]
pub struct SchedulerTexts {
    pub description: String,
}

impl Default for SchedulerTexts {
    fn default() -> Self {
        Self {
            description: session::DESCRIPTION.into(),
        }
    }
}

/// [`install`] with `texts` in force.
pub fn install_with(builder: &mut AgentBuilder, texts: SchedulerTexts) -> Result<(), String> {
    install_inner(builder, texts, SchedulerPlugin::new)
}

/// [`install_with`] with the flow tests' holds in force (feature `testing`).
#[cfg(feature = "testing")]
pub fn install_with_testing(
    builder: &mut AgentBuilder,
    texts: SchedulerTexts,
    holds: std::sync::Arc<testing::Holds>,
) -> Result<(), String> {
    install_inner(builder, texts, move |names, texts| {
        SchedulerPlugin::new_with_testing(names, texts, holds.clone())
    })
}

/// The installation both entry points share: the two plugin constructors
/// differ only by the flow tests' holds, so the registration and the
/// optional-plugin wiring live here once.
fn install_inner(
    builder: &mut AgentBuilder,
    texts: SchedulerTexts,
    make: impl Fn(&Names<'_>, std::sync::Arc<SchedulerTexts>) -> rutis::Result<SchedulerPlugin>
    + Send
    + Sync
    + 'static,
) -> Result<(), String> {
    let texts = std::sync::Arc::new(texts);
    builder
        .catalog_mut()
        .register_shared(dim_agent_runtime::names::name_of::<ScheduleJobs>())
        .register_shared(dim_agent_runtime::names::name_of::<Wakeups>())
        .register_shared(dim_agent_runtime::names::name_of::<Loops>());
    let deliveries = methods::DeliveryHolds::default();
    builder.optional_plugin(move |names| {
        let mut plugin = make(names, texts.clone())?;
        plugin.deliveries = deliveries.clone();
        Ok(Box::new(plugin) as Box<dyn DynPlugin>)
    });
    Ok(())
}

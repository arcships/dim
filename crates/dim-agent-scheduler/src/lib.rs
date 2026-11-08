//! Scheduled jobs for Dim, implemented natively. The design is
//! [docs/design/plugins/Scheduler.md](../../../docs/design/plugins/Scheduler.md).
//!
//! The process owner keeps jobs in the `schedule` settings document and run
//! history in `scheduler/runs.json`, and serves the `schedule` management
//! namespace.
mod document;
mod engine;
mod methods;
mod runs;
mod session;
mod wakeups;
mod watch;

#[cfg(test)]
extern crate self as dim_agent_scheduler;
#[cfg(test)]
#[path = "../tests/support/binding_flow.rs"]
mod binding_flow;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;
#[cfg(test)]
#[path = "../tests/support/wakeup_pass.rs"]
mod wakeup_pass;

use dim_agent_runtime::AgentBuilder;
use rutis::host::DynPlugin;

pub use document::ScheduleDocument;
pub use methods::{ScheduleJobs, SchedulerPlugin};
pub use runs::{RUNS_FILE, SchedulerError};
pub use session::SchedulerSession;
pub use wakeups::Wakeups;

/// Installs the scheduler as an optional feature: the process owner and the
/// `schedule` tool. Neither is required, so a host without the App Server
/// services it needs (settings, pluginFiles, managementMethods and
/// sessionPermissions and sessions), or with an unreadable run store, still starts and
/// opens Sessions; only `schedule.*` and the tool are unavailable.
pub fn install(builder: &mut AgentBuilder) {
    install_with(builder, SchedulerTexts::default()).expect("the built-in texts are valid");
}

/// The policy texts a profile may replace (design §5.1): the `schedule`
/// declaration's description and the report's guidance line.
#[derive(Clone, Debug)]
pub struct SchedulerTexts {
    pub description: String,
    pub report_guidance: String,
}

impl Default for SchedulerTexts {
    fn default() -> Self {
        Self {
            description: session::DESCRIPTION.into(),
            report_guidance: engine::REPORT_GUIDANCE.into(),
        }
    }
}

/// [`install`] with `texts` in force. A `report_guidance` with a line break
/// is refused, since the guide entry tells the model the report's last line
/// is guidance.
pub fn install_with(builder: &mut AgentBuilder, texts: SchedulerTexts) -> Result<(), String> {
    if texts.report_guidance.contains(['\n', '\r']) {
        return Err("Scheduler report_guidance must be one line".into());
    }
    let texts = std::sync::Arc::new(texts);
    dim_agent_runtime::names::register::<ScheduleJobs>(builder.catalog_mut(), None);
    dim_agent_runtime::names::register::<Wakeups>(builder.catalog_mut(), None);
    builder.optional_plugin(move |names| {
        Ok(Box::new(SchedulerPlugin::new(names, texts.clone())?) as Box<dyn DynPlugin>)
    });
    Ok(())
}

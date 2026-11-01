//! Hooks: the documents, their sources and the commands they run
//! (`docs/design/plugins/Hooks.md`).
//!
//! This module holds what both owners need: the document format and its
//! validation, the effective catalog of one hook point, and the Session port
//! that reads the sources. The configuration owner is HooksSettings in App
//! Server; it registers the documents and binds [`HookSourcesHost`].
pub mod document;
mod runner;
mod sources;

pub(crate) use runner::HooksRuntime;
pub use runner::{HooksBranchPlugin, HooksSessionPlugin};

pub use document::{
    HookEntry, HookEvent, HookGroup, HookHandler, HookState, HooksDocument, OnFailure, entries,
    handlers,
};
pub use sources::{
    HookSourceId, HookSourceSnapshot, HookSourceState, HookSourcesHost, HookSourcesSnapshot,
    NoHookSources, ReadHookSources,
};

/// Registers the Hooks parts in every Session and branch.
pub fn install(builder: &mut dim_agent_runtime::AgentBuilder) {
    crate::register_names(builder);
    builder.require_feature(
        dim_agent_runtime::Feature::new("Hooks")
            .session("HooksSession", |names| {
                Ok(Box::new(runner::HooksSessionPlugin::new(names)?))
            })
            .branch("HooksBranch", |names| {
                Ok(Box::new(runner::HooksBranchPlugin::new(names)?))
            }),
    );
}

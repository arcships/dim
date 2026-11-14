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

/// The settle signal flow tests wait on before an unload. Feature `testing`
/// only: a production build has no watch, no event and no state cell.
#[cfg(feature = "testing")]
pub mod testing {
    use super::runner::HookResult;
    use super::sources::HookSourceId;
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };
    use tokio::sync::oneshot;
    type Watch = (PathBuf, String, oneshot::Sender<String>);
    /// The watches armed for a run, by working directory and handler source:
    /// the tests of one process run in parallel, so the key is what tells
    /// them apart.
    static WATCHES: Mutex<Vec<Watch>> = Mutex::new(Vec::new());

    /// Waits for the runner to take one run's result.
    pub struct SettledRun {
        settled: oneshot::Receiver<String>,
    }
    impl SettledRun {
        /// Resolves with the run's reason once the runner has taken its
        /// result. The runner has taken this run's result: an unload after
        /// this cannot replace it.
        pub async fn settled(&mut self) -> String {
            (&mut self.settled)
                .await
                .unwrap_or_else(|_| "the watch was dropped".into())
        }
    }

    /// Watches the next `source` run in the Session working in `cwd`.
    pub fn watch_settled(cwd: &Path, source: &str) -> SettledRun {
        let (settle, settled) = oneshot::channel();
        WATCHES.lock().unwrap().push((
            cwd.canonicalize().expect("an existing working directory"),
            source.to_owned(),
            settle,
        ));
        SettledRun { settled }
    }

    /// Settles the watch armed for this run, if there is one; a watch whose
    /// run never reaches this point never resolves.
    pub(super) fn settle(cwd: &Path, source: &HookSourceId, result: &HookResult) {
        let Ok(cwd) = cwd.canonicalize() else {
            return;
        };
        let source = source.to_string();
        let watch = {
            let mut watches = WATCHES.lock().unwrap();
            let index = watches
                .iter()
                .position(|(watched, held, _)| watched == &cwd && held == &source);
            index.map(|index| watches.swap_remove(index))
        };
        if let Some((_, _, settle)) = watch {
            let status = match result {
                HookResult::Success { .. } => "success".into(),
                HookResult::Blocked { reason } => format!("blocked: {reason}"),
                HookResult::Failed { reason, .. } => reason.clone(),
            };
            let _ = settle.send(status);
        }
    }
}

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

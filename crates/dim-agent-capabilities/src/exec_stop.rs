//! The unified background-task stop port.
//!
//! ShellSession provides `dyn ExecStop` for stopping the minimal profile's
//! shell tasks. It depends on BackgroundExec for recovery;
//! live PTY cancellation remains owned by ShellSession. The local profile
//! uses BackgroundExec's own service and face for native-process stops.

use dim_agent_types::ExecEndedData;
use rutis::host::BoxFuture;
use std::sync::Arc;

/// Stop-path errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskError {
    Unknown,
    Closed,
    Read,
}

/// Stops a background task by id.
pub trait ExecStop: Send + Sync + 'static {
    fn stop(
        self: Arc<Self>,
        task_id: String,
    ) -> BoxFuture<'static, Result<ExecEndedData, TaskError>>;
}

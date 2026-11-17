use dim_agent_capabilities::host::{HostError, ToolExecutionContext};
use dim_agent_types::ToolResult;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// One branch's program executor, as the Session tool sees it.
pub trait CodeModeRunner: Send + Sync + 'static {
    /// Runs one program against the branch's pinned tools. The caller owns
    /// the outer call and its cancellation; every child call uses the same
    /// request identity.
    fn run(
        &self,
        outer_call_id: String,
        code: String,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, HostError>> + Send + '_>>;
}

/// A branch id is session-local (`main` is every Session's default), so the
/// process map is keyed by both ids.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CodeModeRunKey {
    pub session_id: String,
    pub branch_id: String,
}

impl CodeModeRunKey {
    pub fn new(session_id: impl Into<String>, branch_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            branch_id: branch_id.into(),
        }
    }
}

/// The branch runners of one process. A branch part inserts its runner when
/// it mounts and removes it when it closes; the Session tool resolves the
/// caller's Session and branch here, never from Session state. The program
/// semaphore bounds aggregate QuickJS memory.
pub struct CodeModeRuns {
    runners: Mutex<HashMap<CodeModeRunKey, Arc<dyn CodeModeRunner>>>,
    programs: Arc<Semaphore>,
}

impl CodeModeRuns {
    pub fn new(max_programs: usize) -> Self {
        Self {
            runners: Mutex::new(HashMap::new()),
            programs: Arc::new(Semaphore::new(max_programs.max(1))),
        }
    }

    /// Waits for a program slot; cancellation wins over the wait.
    pub async fn acquire_program(
        &self,
        cancel: &CancellationToken,
    ) -> Result<OwnedSemaphorePermit, HostError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(HostError::Cancelled(
                "Code Mode program cancelled before it started".into(),
            )),
            permit = self.programs.clone().acquire_owned() => permit.map_err(|_| {
                HostError::failed("Code Mode program semaphore is closed")
            }),
        }
    }

    pub fn get(&self, session_id: &str, branch_id: &str) -> Option<Arc<dyn CodeModeRunner>> {
        self.runners
            .lock()
            .unwrap()
            .get(&CodeModeRunKey::new(session_id, branch_id))
            .cloned()
    }

    pub fn insert(&self, key: CodeModeRunKey, runner: Arc<dyn CodeModeRunner>) {
        self.runners.lock().unwrap().insert(key, runner);
    }

    pub fn remove(&self, key: &CodeModeRunKey) {
        self.runners.lock().unwrap().remove(key);
    }
}

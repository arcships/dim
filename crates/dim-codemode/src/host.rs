use dim_agent_capabilities::{
    host::{HostError, ToolExecutionContext},
    tool_binding::ToolBinding,
    tool_executor::{NestedGate, ToolExecutor},
};
use dim_agent_types::ToolDefinition;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The child-call surface a program sees. The production implementation wraps
/// `ToolExecutor`; a test supplies its own.
pub trait NestedToolHost: Send + Sync + 'static {
    /// The declarations the request pinned, without the `codemode` tool.
    fn definitions(&self, request_id: &str) -> Result<Vec<ToolDefinition>, String>;

    /// Opens the gate for one outer call.
    fn open(
        &self,
        parent_call_id: String,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Result<Arc<dyn NestedGate>, HostError>;
}

pub struct ToolExecutorHost {
    executor: Arc<ToolExecutor>,
    binding: Arc<ToolBinding>,
}

impl ToolExecutorHost {
    pub fn new(executor: Arc<ToolExecutor>, binding: Arc<ToolBinding>) -> Self {
        Self { executor, binding }
    }
}

impl NestedToolHost for ToolExecutorHost {
    fn definitions(&self, request_id: &str) -> Result<Vec<ToolDefinition>, String> {
        self.binding.definitions(request_id)
    }

    fn open(
        &self,
        parent_call_id: String,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Result<Arc<dyn NestedGate>, HostError> {
        ToolExecutor::open(&self.executor, parent_call_id, context, cancel)
    }
}

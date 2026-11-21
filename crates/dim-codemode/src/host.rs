use dim_agent_capabilities::{
    host::{HostError, HostFuture, ToolExecutionContext},
    tool_binding::ToolBinding,
    tool_executor::ToolExecutor,
};
use dim_agent_types::{ToolCall, ToolDefinition, ToolEffect, ToolResult};
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
    ) -> Result<String, HostError>;
    fn invoke<'a>(&'a self, gate_id: &'a str, call: ToolCall) -> HostFuture<'a, ToolResult>;
    fn close<'a>(&'a self, gate_id: &'a str) -> HostFuture<'a, Vec<ToolEffect>>;
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
    ) -> Result<String, HostError> {
        ToolExecutor::open(&self.executor, parent_call_id, context, cancel)
    }
    fn invoke<'a>(&'a self, gate_id: &'a str, call: ToolCall) -> HostFuture<'a, ToolResult> {
        Box::pin(self.executor.invoke(gate_id, call))
    }
    fn close<'a>(&'a self, gate_id: &'a str) -> HostFuture<'a, Vec<ToolEffect>> {
        Box::pin(self.executor.close(gate_id))
    }
}

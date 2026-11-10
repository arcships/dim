use crate::{config::CodeModeConfig, host::NestedToolHost};
use dim_agent_capabilities::{
    host::{HostError, ToolExecutionContext},
    tool_executor::NestedGate,
};
use dim_agent_types::{JsonObject, ToolCall, ToolEffect, ToolResult};
use rquickjs::{AsyncContext, AsyncRuntime, Function, Object, Promise, prelude::*};
use serde_json::json;
use std::{
    collections::HashSet,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::Semaphore,
    time::{Instant, timeout_at},
};
use tokio_util::sync::CancellationToken;

const CODEMODE_TOOL: &str = "codemode";
const EFFECTS_WARNING: &str =
    "Some child calls may have completed; check their effects before retrying.";

const TOOLS_FACTORY: &str = r#"
(function (__host) {
  const resultText = (content) => {
    if (typeof content === "string") return content;
    if (Array.isArray(content)) {
      return content
        .map((block) => (block && block.type === "text" ? block.text : ""))
        .join("\n");
    }
    return "";
  };
  return {
    list: () => JSON.parse(__host.list()),
    describe: (name) => {
      const value = __host.describe(String(name));
      return value === null ? undefined : JSON.parse(value);
    },
    call: async (name, args) => {
      const value = JSON.parse(await __host.call(String(name), JSON.stringify(args ?? {})));
      if (value.isError) {
        const error = new Error(value.message || resultText(value.content) || "tool failed");
        error.name = "ToolCallError";
        error.code = value.error || "tool_error";
        if (value.structuredContent !== undefined) error.data = value.structuredContent;
        throw error;
      }
      return value;
    },
  };
})
"#;

/// Runs one program in a fresh context. The context receives only the
/// `tools` object; every child call goes through the branch's `ToolExecutor`
/// with its pinned binding and Approval.
pub(crate) async fn run_program(
    executor: Arc<dyn NestedToolHost>,
    config: Arc<CodeModeConfig>,
    outer_call_id: String,
    code: String,
    context: ToolExecutionContext,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    if code.len() > config.max_code_bytes {
        return Ok(ToolResult::error(format!(
            "Code Mode program exceeds the {} byte code limit",
            config.max_code_bytes
        )));
    }
    let timeout = Duration::from_millis(config.max_runtime_ms);
    let max_calls = config.max_calls;
    let deadline = Instant::now() + timeout;

    let definitions = match executor.definitions(&context.request_id) {
        Ok(definitions) => definitions
            .into_iter()
            .filter(|tool| tool.name != CODEMODE_TOOL)
            .collect::<Vec<_>>(),
        Err(message) => return Ok(ToolResult::error(message)),
    };
    let allowed = Arc::new(
        definitions
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<HashSet<_>>(),
    );
    let definitions = Arc::new(definitions);
    let calls = Arc::new(AtomicU64::new(0));
    let parallel = Arc::new(Semaphore::new(config.max_parallel_calls.max(1)));
    let child_cancel = cancel.child_token();
    let gate = match executor.open(outer_call_id, context.clone(), child_cancel.clone()) {
        Ok(gate) => gate,
        Err(error) => return Ok(ToolResult::error(error.to_string())),
    };

    let runtime =
        AsyncRuntime::new().map_err(|error| HostError::failed(format!("runtime: {error}")))?;
    runtime.set_memory_limit(config.max_memory_bytes).await;
    let interrupted = child_cancel.clone();
    runtime
        .set_interrupt_handler(Some(Box::new(move || {
            instant_passed(deadline) || interrupted.is_cancelled()
        })))
        .await;

    let context_handle = AsyncContext::full(&runtime)
        .await
        .map_err(|error| HostError::failed(format!("context: {error}")))?;
    let output_limit = config.max_output_bytes;
    let cancel_after = cancel.clone();
    let calls_after = calls.clone();
    let gate_after = gate.clone();
    let child_cancel_after = child_cancel.clone();
    let run = context_handle.async_with(async move |ctx| {
        let host_tools = Object::new(ctx.clone()).map_err(js_error)?;

        let list_definitions = definitions.clone();
        let list = Function::new(ctx.clone(), move || -> rquickjs::Result<String> {
            let list = list_definitions
                .iter()
                .map(|tool| json!({"name": tool.name, "description": tool.description}))
                .collect::<Vec<_>>();
            Ok(serde_json::to_string(&list).expect("tool summaries serialize"))
        })
        .map_err(js_error)?;
        host_tools.set("list", list).map_err(js_error)?;

        let describe_definitions = definitions.clone();
        let describe = Function::new(
            ctx.clone(),
            move |name: String| -> rquickjs::Result<Option<String>> {
                Ok(describe_definitions
                    .iter()
                    .find(|tool| tool.name == name)
                    .and_then(|tool| serde_json::to_string(tool).ok()))
            },
        )
        .map_err(js_error)?;
        host_tools.set("describe", describe).map_err(js_error)?;

        let call_scope = CallScope {
            gate: gate.clone(),
            cancel: child_cancel.clone(),
            count: calls.clone(),
            parallel: parallel.clone(),
            allowed: allowed.clone(),
            deadline,
            max_calls,
            single_limit: config.max_single_result_bytes,
        };
        let call = Function::new(
            ctx.clone(),
            Async(move |name: String, arguments: String| {
                call_nested_tool(call_scope.clone(), name, arguments)
            }),
        )
        .map_err(js_error)?;
        host_tools.set("call", call).map_err(js_error)?;

        let factory: Function = ctx.eval(TOOLS_FACTORY).map_err(js_error)?;
        let tools: Object = factory.call((host_tools,)).map_err(js_error)?;

        let program = format!(
            r#"
(async function(tools) {{
  const __value = await (async () => {{
{code}
  }})();
  return JSON.stringify(__value === undefined ? null : __value);
}})
"#
        );
        let runner: Function = ctx
            .eval(program)
            .map_err(|error| rquickjs::CaughtError::from_error(&ctx, error).to_string())?;
        let promise: Promise = runner.call((tools,)).map_err(js_error)?;
        match promise.into_future::<String>().await {
            Ok(output) => Ok(output),
            Err(error) => Err(rquickjs::CaughtError::from_error(&ctx, error).to_string()),
        }
    });
    let result = tokio::select! {
        biased;
        _ = cancel_after.cancelled() => None,
        result = timeout_at(deadline, run) => result.ok(),
    };
    child_cancel_after.cancel();
    gate_after.close().await;
    let effects = gate_after.effects();

    match result {
        Some(Ok(output)) => Ok(if output.len() > output_limit {
            let warn = calls_after.load(Ordering::SeqCst) > 0 || !effects.is_empty();
            failure(
                format!("Code Mode output exceeds the {output_limit} byte limit"),
                effects,
                warn,
            )
        } else {
            with_effects(ToolResult::text(output), effects)
        }),
        Some(Err(message)) => {
            let warn = calls_after.load(Ordering::SeqCst) > 0 || !effects.is_empty();
            if cancel_after.is_cancelled() {
                Ok(failure("Code Mode program cancelled", effects, warn))
            } else if instant_passed(deadline) {
                Ok(failure("Code Mode program timed out", effects, warn))
            } else {
                Ok(failure(
                    format!("Code Mode program failed: {message}"),
                    effects,
                    warn,
                ))
            }
        }
        None => {
            let warn = calls_after.load(Ordering::SeqCst) > 0 || !effects.is_empty();
            if cancel_after.is_cancelled() {
                Ok(failure("Code Mode program cancelled", effects, warn))
            } else {
                Ok(failure("Code Mode program timed out", effects, warn))
            }
        }
    }
}

#[derive(Clone)]
struct CallScope {
    gate: Arc<dyn NestedGate>,
    cancel: CancellationToken,
    count: Arc<AtomicU64>,
    parallel: Arc<Semaphore>,
    allowed: Arc<HashSet<String>>,
    deadline: Instant,
    max_calls: u64,
    single_limit: usize,
}

async fn call_nested_tool(
    scope: CallScope,
    name: String,
    arguments: String,
) -> rquickjs::Result<String> {
    let index = scope.count.fetch_add(1, Ordering::SeqCst);
    if index >= scope.max_calls {
        return Ok(error_json(
            "call_limit",
            "the program exceeded its nested-call budget",
        ));
    }
    if !scope.allowed.contains(&name) {
        return Ok(error_json(
            "not_allowed",
            "the tool is not in this request's Code Mode tool list",
        ));
    }
    let permit = tokio::select! {
        biased;
        _ = scope.cancel.cancelled() => {
            return Ok(error_json("cancelled", "the program stopped before the call started"));
        }
        _ = tokio::time::sleep_until(scope.deadline) => {
            return Ok(error_json("timeout", "the call budget expired before the call started"));
        }
        permit = scope.parallel.acquire() => match permit {
            Ok(permit) => permit,
            Err(_) => {
                return Ok(error_json("cancelled", "the program stopped before the call started"));
            }
        },
    };
    let input: JsonObject = match serde_json::from_str(&arguments) {
        Ok(input) => input,
        Err(error) => {
            drop(permit);
            return Ok(error_json("invalid_arguments", &error.to_string()));
        }
    };
    let call = ToolCall {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        input,
    };
    let result = scope.gate.invoke(call).await;
    drop(permit);
    match result {
        Ok(result) => {
            let mut writer = LimitedWriter {
                limit: scope.single_limit,
                bytes: Vec::new(),
            };
            match serde_json::to_writer(&mut writer, &result) {
                Ok(()) => match String::from_utf8(writer.bytes) {
                    Ok(text) => Ok(text),
                    Err(error) => Ok(error_json("invalid_result", &error.to_string())),
                },
                Err(error) => Ok(error_json(
                    "result_too_large",
                    &format!("the tool result exceeded the Code Mode per-call limit: {error}"),
                )),
            }
        }
        Err(error) => Ok(error_json("tool_error", &error.to_string())),
    }
}

struct LimitedWriter {
    limit: usize,
    bytes: Vec<u8>,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(io::Error::other("per-child result limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn with_effects(mut result: ToolResult, effects: Vec<ToolEffect>) -> ToolResult {
    if !effects.is_empty() {
        result.effects = Some(effects);
    }
    result
}

fn failure(message: impl Into<String>, effects: Vec<ToolEffect>, warn: bool) -> ToolResult {
    let message = message.into();
    let text = if warn {
        format!("{message}\n{EFFECTS_WARNING}")
    } else {
        message
    };
    with_effects(ToolResult::error(text), effects)
}

fn instant_passed(deadline: Instant) -> bool {
    Instant::now() >= deadline
}

fn js_error(error: rquickjs::Error) -> String {
    error.to_string()
}

fn error_json(code: &str, message: &str) -> String {
    json!({"isError": true, "error": code, "message": message}).to_string()
}

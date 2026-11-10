use crate::{CodeModeConfig, NestedToolHost, interpreter::run_program};
use dim_agent_capabilities::{
    host::{HostError, ToolExecutionContext},
    tool_executor::NestedGate,
    tool_registry::definition,
};
use dim_agent_types::{ToolCall, ToolContent, ToolDefinition, ToolResult, ToolSource};
use serde_json::json;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

struct MockHost {
    definitions: Vec<ToolDefinition>,
}

impl NestedToolHost for MockHost {
    fn definitions(&self, _: &str) -> Result<Vec<ToolDefinition>, String> {
        Ok(self.definitions.clone())
    }

    fn open(
        &self,
        _: String,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> Result<Arc<dyn NestedGate>, HostError> {
        Ok(Arc::new(MockGate {
            effects: std::sync::Mutex::new(Vec::new()),
        }))
    }
}

struct MockGate {
    effects: std::sync::Mutex<Vec<dim_agent_types::ToolEffect>>,
}

impl NestedGate for MockGate {
    fn invoke(
        &self,
        call: ToolCall,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, HostError>> + Send + '_>> {
        Box::pin(async move {
            let result = match call.name.as_str() {
                "echo" => Ok(ToolResult::text(
                    serde_json::to_string(&call.input).expect("input serializes"),
                )),
                "fail" => Ok(ToolResult::error("boom")),
                "write" => {
                    let mut result = ToolResult::text("wrote");
                    result.effects = Some(vec![dim_agent_types::ToolEffect::FileWritten {
                        path: "/tmp/codemode".into(),
                    }]);
                    Ok(result)
                }
                "blocks" => Ok(ToolResult {
                    content: ToolContent::Blocks(vec![
                        dim_agent_types::ContentBlock::Text {
                            text: "not found".into(),
                        },
                        dim_agent_types::ContentBlock::Text {
                            text: r#"{"code":"missing"}"#.into(),
                        },
                    ]),
                    is_error: Some(true),
                    ..ToolResult::text("")
                }),
                "slow" => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(ToolResult::text("slow"))
                }
                other => Ok(ToolResult::error(format!("unknown tool {other}"))),
            }?;
            if let Some(effects) = result.effects.clone() {
                self.effects.lock().unwrap().extend(effects);
            }
            Ok(result)
        })
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    fn effects(&self) -> Vec<dim_agent_types::ToolEffect> {
        self.effects.lock().unwrap().clone()
    }
}

fn context() -> ToolExecutionContext {
    ToolExecutionContext {
        turn_id: "turn".into(),
        request_id: "request".into(),
        branch_id: "branch".into(),
    }
}

fn host() -> Arc<MockHost> {
    let mut codemode = definition("codemode", "self", json!({"type": "object"}));
    codemode.source = ToolSource::Extension;
    Arc::new(MockHost {
        definitions: vec![
            definition("echo", "echo", json!({"type": "object"})),
            definition("fail", "fail", json!({"type": "object"})),
            definition("write", "write", json!({"type": "object"})),
            definition("slow", "slow", json!({"type": "object"})),
            definition("blocks", "blocks", json!({"type": "object"})),
            codemode,
        ],
    })
}

async fn run_with(config: CodeModeConfig, code: &str) -> ToolResult {
    run_program(
        host(),
        Arc::new(config),
        "outer".into(),
        code.into(),
        context(),
        CancellationToken::new(),
    )
    .await
    .expect("program settles")
}

async fn run(code: &str) -> ToolResult {
    run_with(CodeModeConfig::default(), code).await
}

fn text(result: &ToolResult) -> &str {
    match &result.content {
        ToolContent::Text(text) => text,
        ToolContent::Blocks(_) => panic!("expected text"),
    }
}

#[tokio::test]
async fn calls_a_pinned_tool_and_returns_its_whole_result() {
    let result = run(r#"const value = await tools.call("echo", {x: 1}); return value;"#).await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(value, json!({"content": "{\"x\":1}"}));
}

#[tokio::test]
async fn lists_only_request_tools() {
    let result = run("return tools.list();").await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    let names = value
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["echo", "fail", "write", "slow", "blocks"]);
}

#[tokio::test]
async fn a_block_error_message_keeps_its_blocks_apart() {
    let result = run(
        r#"try { await tools.call("blocks", {}); } catch (error) { return {message: error.message}; }"#,
    )
    .await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(value["message"], "not found\n{\"code\":\"missing\"}");
}

#[tokio::test]
async fn a_tool_error_is_a_catchable_program_error() {
    let result = run(
        r#"try { await tools.call("fail", {}); } catch (error) { return {code: error.code}; }"#,
    )
    .await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(value["code"], "tool_error");
}

#[tokio::test]
async fn the_call_budget_is_enforced() {
    let result = run_with(
        CodeModeConfig {
            max_calls: 1,
            ..Default::default()
        },
        r#"const values = [];
            for (let i = 0; i < 2; i++) {
              try { values.push(await tools.call("echo", {i})); }
              catch (error) { values.push(error.code); }
            }
            return values;"#,
    )
    .await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(value[0]["content"], "{\"i\":0}");
    assert_eq!(value[1], "call_limit");
}

#[tokio::test]
async fn child_effects_reach_the_outer_result() {
    let result = run(r#"await tools.call("write", {}); return "ok";"#).await;
    let effects = result.effects.expect("effects");
    assert_eq!(
        effects,
        vec![dim_agent_types::ToolEffect::FileWritten {
            path: "/tmp/codemode".into()
        }]
    );
}

#[tokio::test]
async fn an_unawaited_child_effect_reaches_the_outer_result() {
    let result =
        run(r#"tools.call("write", {}); await tools.call("slow", {millis: 50}); return "ok";"#)
            .await;
    assert_eq!(
        result.effects,
        Some(vec![dim_agent_types::ToolEffect::FileWritten {
            path: "/tmp/codemode".into()
        }])
    );
}

#[tokio::test]
async fn a_never_settling_promise_is_a_timeout() {
    let config = CodeModeConfig {
        max_runtime_ms: 50,
        ..Default::default()
    };
    let result = run_with(config, "await new Promise(() => {}); return 1;").await;
    assert!(text(&result).starts_with("Code Mode program timed out"));
    assert!(!text(&result).contains("Some child calls may have completed"));
}

#[tokio::test]
async fn a_javascript_error_keeps_its_message() {
    let result = run(r#"throw new Error("line one")"#).await;
    assert!(text(&result).contains("line one"), "{}", text(&result));
    assert!(
        !text(&result).contains("Some child calls may have completed"),
        "{}",
        text(&result)
    );
}

#[tokio::test]
async fn a_syntax_error_keeps_its_message() {
    let result = run("return (").await;
    assert!(
        text(&result).contains("Code Mode program failed"),
        "{}",
        text(&result)
    );
    assert!(
        !text(&result).contains("Exception generated by QuickJS"),
        "{}",
        text(&result)
    );
}

#[tokio::test]
async fn the_host_bridge_is_not_a_global() {
    let result = run("return typeof globalThis.__codemodeTools;").await;
    assert_eq!(text(&result), "\"undefined\"");
}

#[tokio::test]
async fn recursive_codemode_is_not_allowed() {
    let result = run(
        r#"try { await tools.call("codemode", {code: "return 1"}); } catch (error) { return error.code; }"#,
    )
    .await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(value, "not_allowed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_wins_over_a_synchronous_loop() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
    });
    let result = run_program(
        host(),
        Arc::new(CodeModeConfig::default()),
        "outer".into(),
        "while (true) {}".into(),
        context(),
        token,
    )
    .await
    .expect("program settles");
    assert!(text(&result).starts_with("Code Mode program cancelled"));
}

#[tokio::test]
async fn a_child_result_over_the_host_limit_is_an_error() {
    let result = run_with(
        CodeModeConfig {
            max_single_result_bytes: 5,
            ..Default::default()
        },
        r#"try { await tools.call("echo", {x: 1}); } catch (error) { return error.code; }"#,
    )
    .await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(value, "result_too_large");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiting_parallel_call_is_released_by_cancellation() {
    let config = CodeModeConfig {
        max_parallel_calls: 1,
        ..Default::default()
    };
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    });
    let result = run_program(
        host(),
        Arc::new(config),
        "outer".into(),
        r#"await Promise.all([tools.call("slow", {}), tools.call("slow", {})]); return "done";"#
            .into(),
        context(),
        token,
    )
    .await
    .expect("program settles");
    assert!(text(&result).starts_with("Code Mode program cancelled"));
}

#[tokio::test]
async fn a_synchronous_loop_is_interrupted() {
    let config = CodeModeConfig {
        max_runtime_ms: 50,
        ..Default::default()
    };
    let result = run_with(config, "while (true) {}").await;
    assert!(text(&result).starts_with("Code Mode program timed out"));
}

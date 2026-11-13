use super::*;
use aimux_core::{
    LanguageModel, language_model_message::LanguageModelMessage, stream_part::StreamPart,
};
use aimux_provider_utils::Resolvable;
use aimux_providers::{
    anthropic::{AnthropicProviderSettings, create_anthropic},
    bedrock::{AmazonBedrockProviderSettings, create_amazon_bedrock},
    google::{GoogleProviderSettings, create_google},
    openai_compatible::{OpenAICompatibleProviderSettings, create_openai_compatible},
};
use futures_util::StreamExt;
use serde_json::json;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
    let request_line = headers.lines().next().unwrap_or_default().to_string();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    while bytes.len() < header_end + length {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
    }
    (
        request_line,
        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
    )
}

async fn send(options: CallOptions, provider: &str) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (line, request) = read_request(&mut socket).await;
        assert_eq!(line, "POST /v1/chat/completions HTTP/1.1");
        let response = concat!(
            "data: {\"id\":\"completion\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"completion\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        request
    });
    let model = create_openai_compatible(OpenAICompatibleProviderSettings {
        name: provider.into(),
        base_url: format!("http://{address}/v1"),
        api_key: Some("test-key".into()),
        include_usage: Some(true),
        ..OpenAICompatibleProviderSettings::default()
    })
    .unwrap()
    .chat("gpt-4o");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut stream = model.do_stream(&options).await.unwrap().stream;
        let mut finished = false;
        while let Some(part) = stream.next().await {
            match part.unwrap() {
                StreamPart::Finish { .. } => finished = true,
                StreamPart::Error { error } => panic!("unexpected provider error: {error}"),
                _ => {}
            }
        }
        assert!(finished);
        server.await.unwrap()
    })
    .await
    .unwrap()
}

/// One request as the OpenAI Chat model sends it to a server that only
/// records it: the model the `openai-chat` protocol builds.
async fn send_chat(options: CallOptions, model_id: &str) -> Value {
    capture(
        |url| {
            Arc::new(
                aimux_providers::openai::create_openai(
                    aimux_providers::openai::OpenAIProviderSettings {
                        base_url: Some(url),
                        api_key: Some(String::new()),
                        ..Default::default()
                    },
                )
                .unwrap()
                .chat(model_id),
            )
        },
        options,
    )
    .await
}

/// One request as the model sends it to a server that only records it.
async fn capture<F>(build: F, options: CallOptions) -> Value
where
    F: FnOnce(String) -> Arc<dyn LanguageModel>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let model = build(format!("http://{address}/v1"));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_line, request) = read_request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        request
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let _ = model.do_stream(&options).await;
        server.await.unwrap()
    })
    .await
    .unwrap()
}

/// [`capture`], also returning the warnings the model raised, so a test can
/// assert the adapter had nothing to clamp.
async fn capture_warnings<F>(build: F, options: CallOptions) -> (Value, Vec<String>)
where
    F: FnOnce(String) -> Arc<dyn LanguageModel>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let model = build(format!("http://{address}/v1"));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_line, request) = read_request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        request
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut stream = model.do_stream(&options).await.unwrap().stream;
        let mut warnings = Vec::new();
        if let Some(Ok(StreamPart::StreamStart { warnings: raised })) = stream.next().await {
            warnings = raised
                .iter()
                .map(|warning| format!("{warning:?}"))
                .collect();
        }
        (server.await.unwrap(), warnings)
    })
    .await
    .unwrap()
}

fn request(config: Value) -> ModelRequest {
    let mut request = ModelRequest::new(
        "application-alias",
        "gpt-4o",
        vec![dim_agent_types::Message::text(Role::User, "hello")],
    );
    if !config.is_null() {
        request.config = Some(serde_json::from_value(config).unwrap());
    }
    request
}

/// The request with a capability snapshot whose reasoning declaration states
/// `budget` as its range, which is what the catalog carries for a model that
/// thinks with a budget.
fn request_declaring(config: Value, model: &str, budget: Option<Value>) -> ModelRequest {
    request_declaring_mode(config, model, budget, Some("budget"), Some(false))
}

/// The request with a capability snapshot stating the model's reasoning
/// declaration outright: its `mode`, whether thinking is on by default, and
/// the budget range it declares.
fn request_declaring_mode(
    config: Value,
    model: &str,
    budget: Option<Value>,
    mode: Option<&str>,
    default_enabled: Option<bool>,
) -> ModelRequest {
    let mut request = request(config);
    request.model_id = model.into();
    let mut declaration = json!({"supported": true, "budget": budget});
    if let Some(mode) = mode {
        declaration["mode"] = json!(mode);
    }
    if let Some(default_enabled) = default_enabled {
        declaration["defaultEnabled"] = json!(default_enabled);
    }
    request.capability_snapshot = Some(
        serde_json::from_value(json!({
            "target": {"providerId": "application-alias", "modelId": model},
            "capabilities": {
                "modalities": ["text"], "toolCalling": true, "streaming": true,
                "structuredOutput": true, "vision": false, "audio": false,
                "reasoning": true, "contextWindow": 1_000_000, "maxOutputTokens": 131_072
            },
            "reasoning": declaration
        }))
        .unwrap(),
    );
    request
}

fn tool(name: &str) -> dim_agent_types::ToolDefinition {
    serde_json::from_value(json!({
        "name": name, "description": "", "source": "dynamic",
        "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"], "additionalProperties": false}
    })).unwrap()
}

#[tokio::test]
async fn final_request_replays_multimodal_tool_history_and_only_model_readable_results() {
    let name = "plugin:notion/notion__notion-list-recent-pages";
    let request: ModelRequest = serde_json::from_value(json!({
        "providerId": "my-openai", "modelId": "gpt-4o",
        "system": [{"type": "text", "text": ""}, {"type": "text", "text": "system\n"}],
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": " hello\n"},
                {"type": "image", "source": {"type": "base64", "mediaType": "image/png", "data": "aGVsbG8="}},
                {"type": "image", "source": {"type": "url", "mediaType": "image/jpeg", "url": "https://example.test/image.jpg"}}
            ]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan", "startTime": "2026-09-21T00:00:00.000Z"},
                {"type": "text", "text": "searching"},
                {"type": "tool_use", "id": "call-1", "name": name, "input": {"query": "recent"}}
            ]},
            {"role": "tool_result", "content": [{"type": "tool_result", "tool_use_id": "call-1", "content": [{"type": "text", "text": "part 1\n"}, {"type": "text", "text": "part 2"}], "is_error": false, "structuredContent": {"private": "structured-only-sentinel"}}]},
            {"role": "user", "content": "continue"}
        ],
        "tools": [tool(name)],
        "config": {"maxTokens": 4096, "temperature": 0, "topP": 0, "topK": 1, "stopSequences": []},
        "metadata": {"sessionId": "session-local", "runId": "run-local", "requestId": "request-local"}
    })).unwrap();
    let prepared = convert(&request, &ProviderKind::OpenaiChat, None).unwrap();
    assert_eq!(
        prepared
            .tool_names
            .original("dim_tool_plugin_notio_ion_notion-list-recent-pages_27e3aae889c3"),
        name
    );
    let LanguageModelMessage::Tool { content, .. } = &prepared.options.prompt[4] else {
        panic!("missing tool message")
    };
    let ToolPart::ToolResult(result) = &content[0] else {
        panic!("missing tool result")
    };
    assert_eq!(
        *result,
        ToolResultPart {
            tool_call_id: "call-1".into(),
            tool_name: "notion-list-recent-pages_27e3aae889c3".into(),
            output: ToolResultOutput::Text {
                value: "part 1\npart 2".into(),
                provider_options: None,
            },
            provider_options: None,
        }
    );
    let body = send_chat(prepared.options, "gpt-4o").await;
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": ""}, {"role": "system", "content": "system\n"},
            {"role": "user", "content": [
                {"type": "text", "text": " hello\n"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,aGVsbG8="}},
                {"type": "image_url", "image_url": {"url": "https://example.test/image.jpg"}}
            ]},
            {"role": "assistant", "content": "searching", "reasoning_content": "plan", "tool_calls": [{"id": "call-1", "type": "function", "function": {"name": "notion-list-recent-pages_27e3aae889c3", "arguments": "{\"query\":\"recent\"}"}}]},
            {"role": "tool", "tool_call_id": "call-1", "content": "part 1\npart 2"},
            {"role": "user", "content": "continue"}
        ])
    );
    assert_eq!(
        body["tools"][0]["function"]["name"],
        "notion-list-recent-pages_27e3aae889c3"
    );
    assert_eq!(
        body["tools"][0]["function"]["parameters"],
        serde_json::to_value(&request.tools.as_ref().unwrap()[0].input_schema).unwrap()
    );
    assert_eq!(body["tools"][0]["function"]["description"], "");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["top_p"], 0.0);
    assert_eq!(body["top_k"], 1.0);
    assert_eq!(body["stop"], json!([]));
    // Both Chat models replay the reasoning text main replayed.
    let compatible = convert(&request, &ProviderKind::OpenaiCompatible, None).unwrap();
    let body = send(compatible.options, "openai").await;
    assert_eq!(body["messages"][3]["reasoning_content"], "plan");
    let body = send_chat(
        convert(&request, &ProviderKind::OpenaiChat, None)
            .unwrap()
            .options,
        "gpt-4o",
    )
    .await;
    assert_eq!(body["messages"][3]["reasoning_content"], "plan");
    assert!(!body.to_string().contains("structured-only-sentinel"));
    for local in [
        "metadata",
        "session_id",
        "run_id",
        "requestId",
        "structuredContent",
        "startTime",
    ] {
        assert!(!body.to_string().contains(local));
    }
}

#[tokio::test]
async fn omitted_settings_and_empty_values_keep_their_request_meaning() {
    for tools in [None, Some(vec![])] {
        let mut request = request(Value::Null);
        request.tools = tools.map(std::sync::Arc::new);
        request.system = Some(Arc::new(SystemPrompt::Parts(vec![])));
        request.messages.clear();
        let prepared = convert(&request, &ProviderKind::OpenaiChat, None).unwrap();
        let body = send_chat(prepared.options, "gpt-4o").await;
        assert_eq!(body["messages"], json!([]));
        for key in [
            "tools",
            "tool_choice",
            "max_tokens",
            "temperature",
            "top_p",
            "top_k",
            "stop",
            "reasoning_effort",
            "service_tier",
        ] {
            assert!(body.get(key).is_none(), "unexpected {key}: {body}");
        }
    }
    let mut request = request(json!({"maxTokens": 4294967295.0, "stopSequences": ["", " \n"]}));
    request.system = Some(Arc::new(SystemPrompt::Text(String::new())));
    request.messages = vec![std::sync::Arc::new(dim_agent_types::Message::text(
        Role::User,
        "",
    ))];
    let body = send_chat(
        convert(&request, &ProviderKind::OpenaiChat, None)
            .unwrap()
            .options,
        "gpt-4o",
    )
    .await;
    assert_eq!(
        body["messages"],
        json!([{"role": "system", "content": ""}, {"role": "user", "content": ""}])
    );
    assert_eq!(body["max_tokens"], u32::MAX);
    assert_eq!(body["stop"], json!(["", " \n"]));
}

#[tokio::test]
async fn reasoning_uses_selected_provider_fields_and_intent_precedence_on_the_wire() {
    let cases = [
        (
            Some("deepseek"),
            json!({"providerOptions": {"deepseek": {"reasoningEffort": "high"}, "openai-compatible": {"thinking": {"type": "disabled"}}}}),
            json!({"reasoning_effort": "high"}),
        ),
        (
            Some("deepseek"),
            json!({"providerOptions": {"deepseek": {"reasoningEffort": "none"}, "openai-compatible": {"thinking": {"type": "enabled"}}}}),
            json!({"reasoning_effort": "none"}),
        ),
        (
            None,
            json!({"reasoning": {"effort": "none"}, "providerOptions": {"openai-compatible": {"thinking": {"type": "enabled"}}}}),
            json!({"thinking": {"type": "enabled"}}),
        ),
        (
            None,
            json!({"reasoning": {"effort": "high"}, "providerOptions": {"openai-compatible": {"reasoningEffort": "provider-default"}}}),
            json!({}),
        ),
        (
            None,
            json!({"reasoning": {"enabled": true}, "providerOptions": {"openai-compatible": {"enable_thinking": false, "serviceTier": "auto"}}}),
            json!({"enable_thinking": false, "service_tier": "auto"}),
        ),
        (
            Some("moonshotai"),
            json!({"reasoning": {"enabled": true, "effort": "low"}, "providerOptions": {"moonshotai": {"reasoningEffort": "high"}, "deepseek": {"interleavedThinking": false}, "openai": {"summary": "auto"}, "openai-compatible": {"thinking": {"type": "enabled"}, "enable_thinking": true}}}),
            json!({"reasoning_effort": "high", "thinking": {"type": "enabled"}, "enable_thinking": true}),
        ),
    ];
    for (preset, config, expected) in cases {
        let prepared = convert(&request(config), &ProviderKind::OpenaiCompatible, preset).unwrap();
        assert!(prepared.options.reasoning.is_none());
        let body = send(prepared.options, preset.unwrap_or("openai")).await;
        for key in [
            "reasoning_effort",
            "thinking",
            "enable_thinking",
            "service_tier",
        ] {
            assert_eq!(body.get(key), expected.get(key), "key={key}; body={body}");
        }
    }
    let request = request(
        json!({"reasoning": {"effort": "low"}, "providerOptions": {"openai": {"reasoningEffort": "high", "serviceTier": "default"}, "openai-compatible": {"interleavedThinking": true}}}),
    );
    let body = send_chat(
        convert(&request, &ProviderKind::OpenaiChat, Some("deepseek"))
            .unwrap()
            .options,
        "gpt-4o",
    )
    .await;
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["service_tier"], "default");
}

#[test]
fn unsupported_request_values_fail_with_model_and_field_before_http() {
    for (config, path) in [
        (json!({"maxTokens": 4294967296.0}), "config.maxTokens"),
        (json!({"maxTokens": 0.5}), "config.maxTokens"),
        (
            json!({"providerOptions": {"openai-compatible": {"reasoningEffort": "high", "enable_thinking": false}}}),
            "config.providerOptions.openai-compatible",
        ),
        (
            json!({"reasoning": {"enabled": true, "effort": "high"}, "providerOptions": {"openai-compatible": {"reasoningEffort": "provider-default"}}}),
            "config.reasoning.enabled",
        ),
        (
            json!({"reasoning": {"budgetTokens": 0}}),
            "config.reasoning.budgetTokens",
        ),
        (
            json!({"reasoning": {"includeThoughts": false}}),
            "config.reasoning.includeThoughts",
        ),
        (
            json!({"reasoning": {"interleavedThinking": false}}),
            "config.reasoning.interleavedThinking",
        ),
        (
            json!({"providerOptions": {"openai-compatible": {"thinking": {}}}}),
            "config.providerOptions.openai-compatible.thinking.type",
        ),
        (
            json!({"providerOptions": {"openai-compatible": {"reasoningEffort": ""}}}),
            "config.providerOptions.openai-compatible",
        ),
        (
            json!({"providerOptions": {"openai-compatible": {"serviceTier": ""}}}),
            "config.providerOptions.openai-compatible.serviceTier",
        ),
    ] {
        let Err(error) = convert(&request(config), &ProviderKind::OpenaiCompatible, None) else {
            panic!("expected error at {path}");
        };
        let message = error.to_string();
        assert!(message.contains("application-alias/gpt-4o"), "{message}");
        assert!(message.contains(path), "{message}");
    }
    for message in [
        json!({"role": "tool_result", "content": "no ID"}),
        json!({"role": "tool_result", "content": [{"type": "tool_result", "tool_use_id": "missing", "content": ""}]}),
        json!({"role": "user", "content": [{"type": "thinking", "thinking": "wrong role"}]}),
    ] {
        let mut request = request(Value::Null);
        request.messages = vec![serde_json::from_value(message).unwrap()];
        assert!(convert(&request, &ProviderKind::OpenaiChat, None).is_err());
    }
    let mut request = request(Value::Null);
    request.system = Some(Arc::new(
        serde_json::from_value(
            json!([{"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}]),
        )
        .unwrap(),
    ));
    assert!(convert(&request, &ProviderKind::OpenaiChat, None).is_err());
}

#[tokio::test]
async fn tool_name_compatibility_preserves_hash_recovery_and_rejects_wire_collisions() {
    let original = "plugin:nowledge-mem/nowledge-mem__trigger_memory_catchup";
    let mut request = request(Value::Null);
    request.tools = Some(vec![
        tool(original),
        tool("other_045e29d516c3"),
        tool("read"),
        tool(&format!("插件:{}工具", "very-long-server/".repeat(8))),
    ])
    .map(std::sync::Arc::new);
    let prepared = convert(&request, &ProviderKind::OpenaiChat, None).unwrap();
    assert_eq!(
        prepared
            .tool_names
            .original("trigger_memory_catchup_045e29d516c3"),
        original
    );
    assert_eq!(
        prepared.tool_names.original("other_045e29d516c3"),
        "other_045e29d516c3"
    );
    assert_eq!(
        prepared.tool_names.original("alias_045e29d516c3"),
        "alias_045e29d516c3"
    );
    assert_eq!(prepared.tool_names.original("unknown_tool"), "unknown_tool");
    let body = send_chat(prepared.options, "gpt-4o").await;
    assert_eq!(
        body["tools"][0]["function"]["name"],
        "trigger_memory_catchup_045e29d516c3"
    );
    assert_eq!(body["tools"][2]["function"]["name"], "read");
    assert_eq!(
        body["tools"][3]["function"]["name"].as_str().unwrap().len(),
        64
    );

    request.tools = Some(vec![
        tool(original),
        tool("trigger_memory_catchup_045e29d516c3"),
    ])
    .map(std::sync::Arc::new);
    let Err(error) = convert(&request, &ProviderKind::OpenaiChat, None) else {
        panic!("expected name collision");
    };
    assert!(error.to_string().contains("tools[1].name"));
}

// The canonical schema every protocol must send for a tool whose input
// schema arrived from MCP with a `type` array on one property.

fn mcp_shaped_tool() -> Value {
    json!({
        "name": "spaces", "description": "", "source": "dynamic",
        "inputSchema": {
            "type": "object",
            "properties": {"space_id": {"type": ["string", "null"]}},
            "required": ["space_id"]
        }
    })
}

fn canonical_shaped_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"space_id": {"anyOf": [{"type": "string"}, {"type": "null"}]}},
        "required": ["space_id"]
    })
}

fn tool_request(tools: Vec<Value>) -> ModelRequest {
    serde_json::from_value(json!({
        "providerId": "application-alias", "modelId": "gpt-4o",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": tools
    }))
    .unwrap()
}

fn options(kind: ProviderKind) -> CallOptions {
    options_with(mcp_shaped_tool(), kind, None)
}

fn options_with(tool: Value, kind: ProviderKind, preset: Option<&str>) -> CallOptions {
    convert(&tool_request(vec![tool]), &kind, preset)
        .unwrap()
        .options
}

/// The same tool with `additionalProperties: false`.
fn mcp_shaped_tool_closed() -> Value {
    let mut tool = mcp_shaped_tool();
    tool["inputSchema"]["additionalProperties"] = json!(false);
    tool
}

#[tokio::test]
async fn the_openai_chat_and_compatible_bodies_carry_the_canonical_tool_schema() {
    for preset in [None, Some("deepseek")] {
        let name = preset.unwrap_or("openai");
        let options = options_with(mcp_shaped_tool(), ProviderKind::OpenaiCompatible, preset);
        let body = capture(
            move |url| {
                Arc::new(
                    create_openai_compatible(OpenAICompatibleProviderSettings {
                        name: name.into(),
                        base_url: url,
                        api_key: Some("test-key".into()),
                        ..OpenAICompatibleProviderSettings::default()
                    })
                    .unwrap()
                    .chat("gpt-4o"),
                )
            },
            options,
        )
        .await;
        assert_eq!(
            body["tools"][0]["function"]["parameters"],
            canonical_shaped_schema(),
            "{preset:?}"
        );
    }
    let options = options(ProviderKind::OpenaiChat);
    let body = capture(
        |url| {
            Arc::new(
                create_openai_compatible(OpenAICompatibleProviderSettings {
                    name: "openai".into(),
                    base_url: url,
                    api_key: Some("test-key".into()),
                    ..OpenAICompatibleProviderSettings::default()
                })
                .unwrap()
                .chat("gpt-4o"),
            )
        },
        options,
    )
    .await;
    assert_eq!(
        body["tools"][0]["function"]["parameters"],
        canonical_shaped_schema()
    );
}

/// Zenmux takes the registry's wire field: `max_completion_tokens`, not the
/// `max_tokens` dim wrote before the upgrade.
#[tokio::test]
async fn a_zenmux_chat_request_carries_max_completion_tokens() {
    let mut options = options(ProviderKind::OpenaiCompatible);
    options.max_output_tokens = Some(1234);
    let body = capture(
        |url| {
            aimux_providers::create_provider(
                "zenmux",
                aimux_providers::PresetSettings {
                    api_key: Some(Resolvable::Value(String::new())),
                    base_url: Some(url),
                    ..aimux_providers::PresetSettings::default()
                },
            )
            .unwrap()
            .language_model("some-model")
            .unwrap()
        },
        options,
    )
    .await;
    assert_eq!(body["max_completion_tokens"], 1234);
    assert!(body.get("max_tokens").is_none());
}

#[test]
fn the_openai_responses_body_carries_the_canonical_tool_schema() {
    let body = aimux_providers::openai::responses::build_responses_request_body(
        "gpt-5",
        &options(ProviderKind::OpenaiResponses),
        false,
    )
    .unwrap()
    .body;
    assert_eq!(body["tools"][0]["parameters"], canonical_shaped_schema());
}

#[tokio::test]
async fn the_anthropic_body_carries_the_canonical_tool_schema() {
    let options = options(ProviderKind::Anthropic);
    let body = capture(
        |url| {
            Arc::new(
                create_anthropic(AnthropicProviderSettings {
                    base_url: Some(url),
                    api_key: Some("test-key".into()),
                    ..AnthropicProviderSettings::default()
                })
                .unwrap()
                .chat("claude-sonnet-4-5"),
            )
        },
        options,
    )
    .await;
    assert_eq!(body["tools"][0]["input_schema"], canonical_shaped_schema());
}

#[tokio::test]
async fn the_xai_body_carries_the_canonical_tool_schema() {
    // dim's xai protocol is served by the OpenAI-compatible Chat model, which
    // passes the stored schema through as it is: the dedicated xAI Chat model
    // that removed `additionalProperties: false` is gone with aimux 0.6.
    let options = options_with(
        mcp_shaped_tool_closed(),
        ProviderKind::OpenaiCompatible,
        Some("xai"),
    );
    let body = capture(
        |url| {
            Arc::new(
                create_openai_compatible(OpenAICompatibleProviderSettings {
                    name: "xai".into(),
                    base_url: url,
                    api_key: Some("test-key".into()),
                    ..OpenAICompatibleProviderSettings::default()
                })
                .unwrap()
                .chat("grok-4"),
            )
        },
        options,
    )
    .await;
    let parameters = &body["tools"][0]["function"]["parameters"];
    let mut expected = canonical_shaped_schema();
    expected["additionalProperties"] = json!(false);
    assert_eq!(parameters, &expected);
}

#[test]
fn the_xai_responses_body_carries_the_canonical_tool_schema_as_stored() {
    let body = aimux_providers::xai::responses::build_responses_request_body(
        "grok-4",
        &options_with(mcp_shaped_tool_closed(), ProviderKind::XaiResponses, None),
        false,
    )
    .unwrap()
    .body;
    let parameters = &body["tools"][0]["parameters"];
    let mut expected = canonical_shaped_schema();
    expected["additionalProperties"] = json!(false);
    assert_eq!(parameters, &expected);
}

#[tokio::test]
async fn the_bedrock_body_carries_the_canonical_tool_schema() {
    let options = options(ProviderKind::Bedrock);
    let body = capture(
        |url| {
            Arc::new(
                create_amazon_bedrock(AmazonBedrockProviderSettings {
                    region: Some("us-east-1".into()),
                    api_key: Some(Resolvable::Value(String::new())),
                    access_key_id: Some("access-key".into()),
                    secret_access_key: Some("secret-key".into()),
                    base_url: Some(url),
                    ..AmazonBedrockProviderSettings::default()
                })
                .unwrap()
                .chat("anthropic.claude-sonnet-4-5-20250929-v1:0"),
            )
        },
        options,
    )
    .await;
    assert_eq!(
        body["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"],
        canonical_shaped_schema()
    );
}

#[tokio::test]
async fn the_gemini_body_carries_the_stored_nullable_type_verbatim() {
    let options = options(ProviderKind::Gemini);
    let body = capture(
        |url| {
            Arc::new(
                create_google(GoogleProviderSettings {
                    base_url: Some(url),
                    api_key: Some("test-key".into()),
                    ..GoogleProviderSettings::default()
                })
                .unwrap()
                .chat("gemini-2.5-pro"),
            )
        },
        options,
    )
    .await;
    let parameters = &body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"];
    assert_eq!(parameters, &canonical_shaped_schema());
    assert_eq!(parameters["required"], json!(["space_id"]));
}

/// A stored reply, the only varying input of #1038's switch table.
fn switch_request(assistant: Value, config: Value) -> ModelRequest {
    let mut request: ModelRequest = serde_json::from_value(json!({
        "providerId": "p",
        "modelId": "other",
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": assistant},
            {"role": "user", "content": "again"}
        ]
    }))
    .unwrap();
    if !config.is_null() {
        request.config = Some(serde_json::from_value(config).unwrap());
    }
    request
}

fn anthropic_thinking_reply() -> Value {
    json!([
        {"type": "thinking", "thinking": "why",
         "providerMetadata": {"protocol": "anthropic", "signature": "signature-from-claude"}},
        {"type": "text", "text": "earlier answer"}
    ])
}

fn thinking_on() -> Value {
    json!({"providerOptions": {"anthropic": {"thinking": {"type": "enabled", "budgetTokens": 2048}}}})
}

/// The assistant message of a built request.
fn replied(prepared: &PreparedRequest) -> &[AssistantPart] {
    prepared
        .options
        .prompt
        .iter()
        .find_map(|message| match message {
            LanguageModelMessage::Assistant { content, .. } => Some(content.as_slice()),
            _ => None,
        })
        .expect("the assistant message is present")
}

fn continuation_options(part: &AssistantPart) -> Option<&SharedProviderOptions> {
    match part {
        AssistantPart::Text(part) => part.provider_options.as_ref(),
        AssistantPart::Reasoning(part) => part.provider_options.as_ref(),
        AssistantPart::ToolCall(part) => part.provider_options.as_ref(),
        _ => None,
    }
}

fn assert_no_continuation_data(content: &[AssistantPart]) {
    for part in content {
        assert!(
            continuation_options(part).is_none(),
            "continuation data reached the request: {part:?}"
        );
    }
}

fn text_of(content: &[AssistantPart]) -> Vec<&str> {
    content
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(part) => Some(part.text.as_str()),
            _ => None,
        })
        .collect()
}

fn is_reasoning(part: &AssistantPart) -> bool {
    matches!(part, AssistantPart::Reasoning(_))
}

/// #1038: a stored reply whose continuation data belongs to another protocol
/// is built as a reply with no data, for every target protocol that has none.
#[test]
fn stored_foreign_continuation_data_builds_as_if_absent() {
    for kind in [
        ProviderKind::OpenaiCompatible,
        ProviderKind::OpenaiChat,
        ProviderKind::Gemini,
    ] {
        let prepared = convert(
            &switch_request(anthropic_thinking_reply(), Value::Null),
            &kind,
            None,
        )
        .unwrap_or_else(|error| panic!("{kind:?}: {error}"));
        let content = replied(&prepared);
        assert_no_continuation_data(content);
        assert!(
            content.iter().any(is_reasoning),
            "{kind:?} keeps unsigned thinking: {content:?}"
        );
        assert_eq!(text_of(content), ["earlier answer"], "{kind:?}");
    }
    // The Responses protocol leaves a thinking part without its own data out.
    let prepared = convert(
        &switch_request(anthropic_thinking_reply(), Value::Null),
        &ProviderKind::OpenaiResponses,
        None,
    )
    .unwrap();
    let content = replied(&prepared);
    assert_no_continuation_data(content);
    assert!(!content.iter().any(is_reasoning), "{content:?}");
    assert_eq!(text_of(content), ["earlier answer"]);

    // A thinking part from the Responses protocol does not block an Anthropic
    // request that has thinking on: the part is left out, its data never sent.
    let prepared = convert(
        &switch_request(
            json!([
                {"type": "thinking", "thinking": "why",
                 "providerMetadata": {"protocol": "openai-responses", "itemId": "rs_1", "encryptedContent": "encrypted"}},
                {"type": "text", "text": "earlier answer"}
            ]),
            thinking_on(),
        ),
        &ProviderKind::Anthropic,
        None,
    )
    .unwrap();
    let content = replied(&prepared);
    assert_no_continuation_data(content);
    assert!(!content.iter().any(is_reasoning), "{content:?}");
    assert_eq!(text_of(content), ["earlier answer"]);

    // Text with metadata from another protocol is sent as plain text.
    for (assistant, kind) in [
        (
            json!([{"type": "text", "text": "earlier answer",
                    "providerMetadata": {"protocol": "openai-responses", "itemId": "msg_1"}}]),
            ProviderKind::OpenaiCompatible,
        ),
        (
            json!([{"type": "text", "text": "earlier answer",
                    "providerMetadata": {"protocol": "gemini", "thoughtSignature": "gemini-signature"}}]),
            ProviderKind::OpenaiChat,
        ),
    ] {
        let prepared = convert(&switch_request(assistant, Value::Null), &kind, None).unwrap();
        let content = replied(&prepared);
        assert_no_continuation_data(content);
        assert_eq!(text_of(content), ["earlier answer"], "{kind:?}");
    }

    // The target protocol's own metadata on a content kind it never produces
    // still fails: stored content cannot reach the request that way.
    for (assistant, kind) in [
        (
            json!([{"type": "text", "text": "answer",
                    "providerMetadata": {"protocol": "anthropic", "signature": "signature"}}]),
            ProviderKind::Anthropic,
        ),
        (
            json!([{"type": "text", "text": "answer",
                    "providerMetadata": {"protocol": "bedrock", "signature": "signature"}}]),
            ProviderKind::Bedrock,
        ),
    ] {
        let Err(error) = convert(&switch_request(assistant, Value::Null), &kind, None) else {
            panic!("{kind:?}: a same-protocol wrong-kind part must fail");
        };
        assert!(
            error.to_string().contains("does not fit this content kind"),
            "{error}"
        );
    }
}

/// #1038: Anthropic leaves stored thinking out when thinking is off, and
/// replays the stored signature when it is on.
#[test]
fn anthropic_replays_stored_thinking_only_with_thinking_on() {
    let prepared = convert(
        &switch_request(anthropic_thinking_reply(), Value::Null),
        &ProviderKind::Anthropic,
        None,
    )
    .unwrap_or_else(|error| panic!("thinking off must build: {error}"));
    let content = replied(&prepared);
    assert_no_continuation_data(content);
    assert!(!content.iter().any(is_reasoning), "{content:?}");
    assert_eq!(text_of(content), ["earlier answer"]);

    let prepared = convert(
        &switch_request(anthropic_thinking_reply(), thinking_on()),
        &ProviderKind::Anthropic,
        None,
    )
    .unwrap();
    let content = replied(&prepared);
    let reasoning = content
        .iter()
        .find_map(|part| match part {
            AssistantPart::Reasoning(part) => Some(&part.provider_options),
            _ => None,
        })
        .expect("thinking is replayed");
    assert_eq!(
        reasoning.as_ref().unwrap()["anthropic"]["signature"],
        json!("signature-from-claude")
    );
    assert_eq!(text_of(content), ["earlier answer"]);
}

/// #1038: Bedrock leaves a stored thinking part out when it has no data for
/// Bedrock, whether it has none at all or another protocol's.
#[test]
fn bedrock_leaves_stored_thinking_without_its_data_out() {
    for assistant in [
        anthropic_thinking_reply(),
        json!([
            {"type": "thinking", "thinking": "why"},
            {"type": "text", "text": "earlier answer"}
        ]),
    ] {
        let prepared = convert(
            &switch_request(assistant, Value::Null),
            &ProviderKind::Bedrock,
            None,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let content = replied(&prepared);
        assert_no_continuation_data(content);
        assert!(!content.iter().any(is_reasoning), "{content:?}");
        assert_eq!(text_of(content), ["earlier answer"]);
    }
}

/// The Gemini model carries the stored JSON Schema as `parametersJsonSchema`;
/// the converter turns the boolean subschemas into object schemas there, so
/// `true` reaches the wire as `{}` and `false` as `{"not": {}}`. Recorded here
/// so a change in the converter is visible; dim does not compensate.
#[tokio::test]
async fn gemini_emits_stored_boolean_subschemas_as_object_schemas() {
    let request = tool_request(vec![json!({
        "name": "flags", "description": "", "source": "dynamic",
        "inputSchema": {"type": "object", "properties": {"x": true, "y": false}}
    })]);
    let options = convert(&request, &ProviderKind::Gemini, None)
        .unwrap()
        .options;
    let body = capture(
        |url| {
            Arc::new(
                create_google(GoogleProviderSettings {
                    base_url: Some(url),
                    api_key: Some("test-key".into()),
                    ..GoogleProviderSettings::default()
                })
                .unwrap()
                .chat("gemini-2.5-pro"),
            )
        },
        options,
    )
    .await;
    let properties =
        &body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["properties"];
    assert_eq!(properties["x"], json!({}));
    assert_eq!(properties["y"], json!({"not": {}}));
}

/// #1466: the output allowance reserves an answer room, and a thinking budget
/// never takes it. The table is the rule, in numbers a reader can check
/// without running the function.
#[test]
fn the_thinking_budget_keeps_the_answer_room_of_the_allowance() {
    use dim_agent_capabilities::context_model::{answer_tokens, thinking_tokens};
    for (allowance, answer, thinking) in [
        (1_u64, 1_u64, 0_u64),
        (2_000, 1_000, 1_000),
        (8_192, 4_096, 4_096),
        (16_384, 4_096, 12_288),
        (64_000, 16_000, 48_000),
        (131_072, 32_768, 98_304),
    ] {
        assert_eq!(answer_tokens(allowance), answer, "answer of {allowance}");
        assert_eq!(
            thinking_tokens(allowance),
            thinking,
            "thinking of {allowance}"
        );
        assert_eq!(
            answer_tokens(allowance) + thinking_tokens(allowance),
            allowance
        );
    }
}

/// A budget above the thinking room is capped by it, so the wire cap stays the
/// allowance and the adapter never clamps the sum. This is the #1466
/// regression: before the split the same request sent `max_tokens + budget`
/// past the adapter's cap and failed before any output.
#[tokio::test]
async fn a_budget_above_the_thinking_room_keeps_the_allowance_and_the_answer() {
    for (model, allowance, thinking) in [
        ("claude-sonnet-4-5", 64_000.0, 48_000),
        ("claude-opus-4-5-20251101", 64_000.0, 48_000),
        ("claude-sonnet-4-6", 128_000.0, 96_000),
    ] {
        let (body, warnings) = anthropic_body(model, allowance, allowance - 4_000.).await;
        assert_eq!(
            body["thinking"]["budget_tokens"],
            json!(thinking),
            "{model}: {body}"
        );
        assert_eq!(
            body["max_tokens"],
            json!(allowance as u64),
            "{model}: the adapter adds the budget back, so the wire cap is the allowance (#1466): {body}"
        );
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("Unsupported")),
            "{model}: with the budget inside the room the adapter clamps nothing, so the request is not failed before output: {warnings:?}"
        );
    }
}

/// A budget below the thinking room is kept as it is: the room is the budget's
/// upper bound, not a replacement for the value the configuration stated.
#[tokio::test]
async fn a_budget_below_the_thinking_room_is_kept() {
    let (body, _) = anthropic_body("claude-sonnet-4-5", 64_000., 8_192.).await;
    assert_eq!(body["thinking"]["budget_tokens"], json!(8192), "{body}");
    assert_eq!(
        body["max_tokens"],
        json!(64_000u64),
        "the adapter adds the budget back, so the wire cap is the allowance (#1466): {body}"
    );
}

/// The split never turns thinking on: a request without a reasoning
/// configuration keeps today's body, on a model that declares a budget and on
/// one whose effort-only request stays adaptive.
#[tokio::test]
async fn no_model_starts_thinking_because_of_the_split() {
    let (body, _) = anthropic_body("claude-sonnet-4-5", 64_000., 0.).await;
    assert_eq!(body["max_tokens"], json!(64_000), "{body}");
    assert!(body.get("thinking").is_none(), "{body}");
    let (body, _) = anthropic_effort("claude-sonnet-4-5", 64_000.).await;
    assert_eq!(body["output_config"], json!({"effort": "high"}), "{body}");
    assert!(body.get("thinking").is_none(), "{body}");
    assert_eq!(body["max_tokens"], json!(64_000), "{body}");
    let (body, _) = anthropic_effort("claude-sonnet-4-6", 128_000.).await;
    assert!(body.get("thinking").is_none(), "{body}");
    assert_eq!(body["max_tokens"], json!(128_000), "{body}");
}

/// Bedrock's Converse body keeps the allowance as `maxTokens` with the capped
/// budget beside it, which that adapter takes as given rather than summed into
/// its cap.
#[tokio::test]
async fn a_bedrock_budget_keeps_the_answer_room() {
    let (body, _) = bedrock_body("claude-sonnet-4-5", 64_000., 60_000.).await;
    assert_eq!(
        body["additionalModelRequestFields"]["thinking"]["budget_tokens"],
        json!(48_000),
        "{body}"
    );
    assert_eq!(
        body["inferenceConfig"]["maxTokens"],
        json!(64_000),
        "{body}"
    );
}

/// Gemini counts thinking inside its cap, so the allowance stays the cap and
/// only the budget is lowered, to the room and to the model's own maximum.
#[tokio::test]
async fn a_gemini_budget_is_the_room_never_above_the_model_maximum() {
    let (body, _) = gemini_body("gemini-2.5-flash", 64_000., 60_000.).await;
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        json!(24_576),
        "{body}"
    );
    assert_eq!(
        body["generationConfig"]["maxOutputTokens"],
        json!(64_000),
        "{body}"
    );
    let (body, _) = gemini_effort("gemini-2.5-flash", 64_000., "none").await;
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        json!(0),
        "{body}"
    );
}

/// An effort-only protocol has no field for a reserve: the allowance stays the
/// cap and nothing else about the request changes.
#[tokio::test]
async fn an_effort_only_protocol_carries_no_budget_at_any_allowance() {
    for allowance in [8192.0, 64000.0] {
        let request = request(json!({"maxTokens": allowance, "reasoning": {"effort": "high"}}));
        let prepared = convert(&request, &ProviderKind::OpenaiCompatible, None).unwrap();
        let body = send(prepared.options, "openai").await;
        assert_eq!(body["max_tokens"], json!(allowance as u64), "{body}");
        assert_eq!(body["reasoning_effort"], json!("high"), "{body}");
        for field in ["thinking", "budget_tokens", "thinkingBudget"] {
            assert!(body.get(field).is_none(), "{field}: {body}");
        }
    }
}

/// Asking for thinking when the allowance cannot hold the protocol's floor
/// fails before HTTP, naming the field the configuration set.
#[test]
fn thinking_below_the_protocol_floor_fails_before_http() {
    for (field, config) in [
        (
            "config.reasoning.enabled",
            json!({"maxTokens": 2000, "reasoning": {"enabled": true}}),
        ),
        (
            "config.reasoning.budgetTokens",
            json!({"maxTokens": 2000, "reasoning": {"budgetTokens": 1500}}),
        ),
    ] {
        let Err(error) = convert(&request(config), &ProviderKind::Anthropic, None) else {
            panic!("expected a failure at {field}");
        };
        let message = error.to_string();
        assert!(message.contains(field), "{field}: {message}");
        assert!(
            message.contains("2000"),
            "the allowance is named: {message}"
        );
    }
}

/// An Anthropic request body carrying `budget` as its budget, or none when
/// `budget` is zero.
async fn anthropic_body(model: &'static str, allowance: f64, budget: f64) -> (Value, Vec<String>) {
    let config = if budget == 0.0 {
        json!({"maxTokens": allowance})
    } else {
        json!({"maxTokens": allowance, "reasoning": {"budgetTokens": budget}})
    };
    capture_warnings(
        move |url| {
            Arc::new(
                create_anthropic(AnthropicProviderSettings {
                    base_url: Some(url),
                    api_key: Some("test-key".into()),
                    ..AnthropicProviderSettings::default()
                })
                .unwrap()
                .chat(model),
            )
        },
        convert(
            &request_declaring(config, model, Some(json!({"min": 1024}))),
            &ProviderKind::Anthropic,
            None,
        )
        .unwrap()
        .options,
    )
    .await
}

/// An Anthropic request body carrying an effort and no budget.
async fn anthropic_effort(model: &'static str, allowance: f64) -> (Value, Vec<String>) {
    capture_warnings(
        move |url| {
            Arc::new(
                create_anthropic(AnthropicProviderSettings {
                    base_url: Some(url),
                    api_key: Some("test-key".into()),
                    ..AnthropicProviderSettings::default()
                })
                .unwrap()
                .chat(model),
            )
        },
        convert(
            &request_declaring(
                json!({"maxTokens": allowance, "reasoning": {"effort": "high"}}),
                model,
                Some(json!({"min": 1024})),
            ),
            &ProviderKind::Anthropic,
            None,
        )
        .unwrap()
        .options,
    )
    .await
}

/// A Bedrock Converse body carrying `budget` as its thinking budget.
async fn bedrock_body(model: &'static str, allowance: f64, budget: f64) -> (Value, Vec<String>) {
    let id = format!("anthropic.{model}-20250929-v1:0");
    capture_warnings(
        move |url| {
            Arc::new(
                create_amazon_bedrock(AmazonBedrockProviderSettings {
                    region: Some("us-east-1".into()),
                    api_key: Some(Resolvable::Value(String::new())),
                    access_key_id: Some("access-key".into()),
                    secret_access_key: Some("secret-key".into()),
                    base_url: Some(url),
                    ..AmazonBedrockProviderSettings::default()
                })
                .unwrap()
                .chat(&id),
            )
        },
        convert(
            &request_declaring(
                json!({"maxTokens": allowance, "reasoning": {"budgetTokens": budget}}),
                model,
                Some(json!({"min": 1024})),
            ),
            &ProviderKind::Bedrock,
            None,
        )
        .unwrap()
        .options,
    )
    .await
}

/// A Gemini body carrying `budget` as its thinking budget, on a model whose
/// declaration states `max` as its own ceiling.
async fn gemini_body(model: &'static str, allowance: f64, budget: f64) -> (Value, Vec<String>) {
    capture_warnings(
        move |url| {
            Arc::new(
                create_google(GoogleProviderSettings {
                    base_url: Some(url),
                    api_key: Some("test-key".into()),
                    ..GoogleProviderSettings::default()
                })
                .unwrap()
                .chat(model),
            )
        },
        convert(
            &request_declaring(
                json!({"maxTokens": allowance, "reasoning": {"budgetTokens": budget}}),
                model,
                Some(json!({"min": 0, "max": 24576})),
            ),
            &ProviderKind::Gemini,
            None,
        )
        .unwrap()
        .options,
    )
    .await
}

/// A Gemini body carrying an effort and no budget.
async fn gemini_effort(model: &'static str, allowance: f64, effort: &str) -> (Value, Vec<String>) {
    capture_warnings(
        move |url| {
            Arc::new(
                create_google(GoogleProviderSettings {
                    base_url: Some(url),
                    api_key: Some("test-key".into()),
                    ..GoogleProviderSettings::default()
                })
                .unwrap()
                .chat(model),
            )
        },
        convert(
            &request_declaring(
                json!({"maxTokens": allowance, "reasoning": {"effort": effort}}),
                model,
                Some(json!({"min": 0, "max": 24576})),
            ),
            &ProviderKind::Gemini,
            None,
        )
        .unwrap()
        .options,
    )
    .await
}

/// Without an output allowance every protocol keeps the path it had before
/// #1466: a configured budget is sent as it is, with nothing to cap it by.
/// Asserted on the conversion, which is where a cap would be set.
#[test]
fn a_request_without_an_allowance_keeps_the_budget_it_states() {
    let prepared = convert(
        &request_declaring(
            json!({"reasoning": {"budgetTokens": 8192}}),
            "claude-sonnet-4-5",
            Some(json!({"min": 1024})),
        ),
        &ProviderKind::Anthropic,
        None,
    )
    .unwrap();
    assert!(
        prepared.options.max_output_tokens.is_none(),
        "no allowance was stated, so none is sent"
    );
    // Bedrock refuses an enable without a budget, as it always has.
    let Err(error) = convert(
        &request_declaring_mode(
            json!({"reasoning": {"enabled": true}}),
            "claude-sonnet-4-5",
            Some(json!({"min": 1024})),
            Some("budget"),
            Some(false),
        ),
        &ProviderKind::Bedrock,
        None,
    ) else {
        panic!("expected Bedrock to refuse an enable without a budget");
    };
    assert!(
        error
            .to_string()
            .contains("requires a reasoning budget for Bedrock"),
        "{error}"
    );
}

/// A configured budget keeps the validation and the message it always had: the
/// room is not the reason a value is refused.
#[test]
fn a_configured_budget_keeps_todays_validation_text() {
    for (budget, expected) in [
        (json!(500), "invalid integer reasoning budget"),
        (json!(2048.5), "invalid integer reasoning budget"),
    ] {
        let Err(error) = convert(
            &request_declaring(
                json!({"maxTokens": 64000, "reasoning": {"budgetTokens": budget}}),
                "claude-sonnet-4-5",
                Some(json!({"min": 1024})),
            ),
            &ProviderKind::Anthropic,
            None,
        ) else {
            panic!("expected {budget} to be refused");
        };
        let message = error.to_string();
        assert!(message.contains(expected), "{budget}: {message}");
        assert!(
            message.contains("config.reasoning.budgetTokens"),
            "{budget}: {message}"
        );
        assert!(!message.contains("an allowance of"), "{budget}: {message}");
    }
}

/// A level-mode model is left alone: a budget beside its level would be
/// refused, so the request keeps the body it had.
#[tokio::test]
async fn a_level_mode_model_is_not_given_a_budget() {
    // A level-mode model that declares a numeric range and states no effort:
    // only the mode check keeps a budget off it, because a budget beside a
    // level is refused and the level is this family's whole control.
    let request = request_declaring_mode(
        json!({"maxTokens": 64000}),
        "gemini-3-flash-preview",
        Some(json!({"min": 0, "max": 24576})),
        Some("level"),
        Some(true),
    );
    let (body, _) = gemini_capture(request, "gemini-3-flash-preview").await;
    assert!(
        body["generationConfig"].get("thinkingConfig").is_none(),
        "a level-mode model states no budget: {body}"
    );
    assert_eq!(
        body["generationConfig"]["maxOutputTokens"],
        json!(64000),
        "{body}"
    );

    // Beside an effort it keeps the level it always sent.
    let request = request_declaring_mode(
        json!({"maxTokens": 64000, "reasoning": {"enabled": true, "effort": "high"}}),
        "gemini-3-flash-preview",
        Some(json!({"min": 0, "max": 24576})),
        Some("level"),
        Some(true),
    );
    let (body, _) = gemini_capture(request, "gemini-3-flash-preview").await;
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "high"}),
        "{body}"
    );
    assert_eq!(
        body["generationConfig"]["maxOutputTokens"],
        json!(64000),
        "{body}"
    );
}

/// A budget-mode model whose declaration says thinking is on sends the room as
/// its thinking budget even when the configuration states none, so thinking
/// cannot spend the answer's share.
#[tokio::test]
async fn a_budget_mode_model_that_thinks_sends_the_room() {
    let request = request_declaring_mode(
        json!({"maxTokens": 64000}),
        "gemini-2.5-flash",
        Some(json!({"min": 0, "max": 24576})),
        Some("budget"),
        Some(true),
    );
    let (body, _) = gemini_capture(request, "gemini-2.5-flash").await;
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        json!(24576),
        "the room is 48,000, lowered to the declared maximum: {body}"
    );
    assert_eq!(
        body["generationConfig"]["maxOutputTokens"],
        json!(64000),
        "{body}"
    );

    // An effort is a thinking level, which a budget cannot sit beside: the
    // level stays alone.
    let request = request_declaring_mode(
        json!({"maxTokens": 64000, "reasoning": {"effort": "high"}}),
        "gemini-2.5-flash",
        Some(json!({"min": 0, "max": 24576})),
        Some("budget"),
        Some(true),
    );
    let (body, _) = gemini_capture(request, "gemini-2.5-flash").await;
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "high"}),
        "{body}"
    );

    // A model whose declaration says thinking is off by default is left alone.
    let request = request_declaring_mode(
        json!({"maxTokens": 64000}),
        "gemini-2.5-flash",
        Some(json!({"min": 0, "max": 24576})),
        Some("budget"),
        Some(false),
    );
    let (body, _) = gemini_capture(request, "gemini-2.5-flash").await;
    assert!(
        body["generationConfig"].get("thinkingConfig").is_none(),
        "{body}"
    );
}

/// A Gemini body for `request` as it reaches the wire.
async fn gemini_capture(request: ModelRequest, model: &'static str) -> (Value, Vec<String>) {
    capture_warnings(
        move |url| {
            Arc::new(
                create_google(GoogleProviderSettings {
                    base_url: Some(url),
                    api_key: Some("test-key".into()),
                    ..GoogleProviderSettings::default()
                })
                .unwrap()
                .chat(model),
            )
        },
        convert(&request, &ProviderKind::Gemini, None)
            .unwrap()
            .options,
    )
    .await
}

/// The split never turns thinking off or on: a derived room below the model's
/// declared floor sends no budget, so the model keeps its own default.
#[tokio::test]
async fn a_derived_gemini_room_below_the_floor_sends_no_budget() {
    // A floor of 128 with an allowance of 200: the room is 100, under the floor.
    let request = request_declaring_mode(
        json!({"maxTokens": 200}),
        "gemini-2.5-pro",
        Some(json!({"min": 128, "max": 32768})),
        Some("budget"),
        Some(true),
    );
    let (body, _) = gemini_capture(request, "gemini-2.5-pro").await;
    assert!(
        body["generationConfig"].get("thinkingConfig").is_none(),
        "the room is 100, below the floor of 128: {body}"
    );

    // An allowance of 1 gives a room of 0, and `0` would turn thinking off.
    let request = request_declaring_mode(
        json!({"maxTokens": 1}),
        "gemini-2.5-flash",
        Some(json!({"min": 0, "max": 24576})),
        Some("budget"),
        Some(true),
    );
    let (body, _) = gemini_capture(request, "gemini-2.5-flash").await;
    assert!(
        body["generationConfig"].get("thinkingConfig").is_none(),
        "a room of 0 is not a budget: {body}"
    );

    // A floor the catalog states as 0 still leaves thinking on: no model turns
    // thinking off because of the split, and the floor is at least 1.
    let request = request_declaring_mode(
        json!({"maxTokens": 2}),
        "gemini-2.5-flash",
        Some(json!({"min": 0, "max": 24576})),
        Some("budget"),
        Some(true),
    );
    let (body, _) = gemini_capture(request, "gemini-2.5-flash").await;
    assert!(
        body["generationConfig"].get("thinkingConfig").is_none()
            || body["generationConfig"]["thinkingConfig"]["thinkingBudget"] != json!(0),
        "thinking is never turned off here: {body}"
    );
}

/// A configured budget the room lowers below the model's floor is refused,
/// naming the allowance; a budget already below the floor keeps its own path.
#[tokio::test]
async fn a_capped_gemini_budget_below_the_floor_is_refused() {
    let request = request_declaring_mode(
        json!({"maxTokens": 200, "reasoning": {"budgetTokens": 4096}}),
        "gemini-2.5-pro",
        Some(json!({"min": 128, "max": 32768})),
        Some("budget"),
        Some(true),
    );
    let prepared = convert(&request, &ProviderKind::Gemini, None);
    let Err(error) = prepared else {
        panic!("expected the capped budget to be refused");
    };
    let message = error.to_string();
    assert!(
        message.contains("config.reasoning.budgetTokens"),
        "the field is named: {message}"
    );
    assert!(message.contains("200"), "the allowance is named: {message}");
    assert!(message.contains("128"), "the floor is named: {message}");

    // A configured budget the room lowers below the floor is refused even when
    // the configured value is itself below the floor: dropping it silently
    // would be the alternative.
    let request = request_declaring_mode(
        json!({"maxTokens": 64, "reasoning": {"budgetTokens": 64}}),
        "gemini-2.5-pro",
        Some(json!({"min": 128, "max": 32768})),
        Some("budget"),
        Some(true),
    );
    let prepared = convert(&request, &ProviderKind::Gemini, None);
    let Err(error) = prepared else {
        panic!("expected the capped budget to be refused");
    };
    let message = error.to_string();
    assert!(
        message.contains("config.reasoning.budgetTokens"),
        "the field is named: {message}"
    );
    assert!(message.contains("64"), "the allowance is named: {message}");
    assert!(message.contains("32"), "the room is named: {message}");
    assert!(message.contains("128"), "the floor is named: {message}");

    // A budget already below the floor is not the room's doing: it keeps
    // today's path, where the adapter owns the value.
    let request = request_declaring_mode(
        json!({"maxTokens": 64000, "reasoning": {"budgetTokens": 64}}),
        "gemini-2.5-pro",
        Some(json!({"min": 128, "max": 32768})),
        Some("budget"),
        Some(true),
    );
    let (body, failure) = gemini_capture_or_error(request, "gemini-2.5-pro").await;
    assert!(
        failure.is_none(),
        "a compact budget keeps its own path: {failure:?}"
    );
    assert_eq!(
        body.map(|body| body["generationConfig"]["thinkingConfig"].clone()),
        Some(json!({"thinkingBudget": 64})),
        "a configured value below the floor is sent as it is"
    );
}

/// Gemini's own off and dynamic settings are sent as they are: they skip the
/// cap path entirely, so neither is narrowed through the room.
#[tokio::test]
async fn gemini_off_and_dynamic_budgets_are_sent_as_configured() {
    for (configured, sent) in [(-1.0, -1), (0.0, 0)] {
        let request = request_declaring_mode(
            json!({"maxTokens": 64000, "reasoning": {"budgetTokens": configured}}),
            "gemini-2.5-flash",
            Some(json!({"min": 0, "max": 24576})),
            Some("budget"),
            Some(true),
        );
        let (body, _) = gemini_capture(request, "gemini-2.5-flash").await;
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            json!(sent),
            "{configured}: {body}"
        );
    }
}

/// A native Gemini budget is sent as the configuration states it, the same as
/// Anthropic's native budget: the split neither caps nor derives over it.
#[tokio::test]
async fn a_native_gemini_budget_is_not_capped() {
    let request = request_declaring_mode(
        json!({"maxTokens": 64000, "providerOptions": {"gemini": {"thinkingConfig": {"thinkingBudget": 30000}}}}),
        "gemini-2.5-flash",
        Some(json!({"min": 0, "max": 24576})),
        Some("budget"),
        Some(true),
    );
    let (body, _) = gemini_capture(request, "gemini-2.5-flash").await;
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        json!(30000),
        "the native value stands above both the room and the declared maximum: {body}"
    );
}

/// A Gemini body, or the failure the builder raised before HTTP.
async fn gemini_capture_or_error(
    request: ModelRequest,
    model: &'static str,
) -> (Option<Value>, Option<String>) {
    let prepared = convert(&request, &ProviderKind::Gemini, None);
    match prepared {
        Ok(prepared) => {
            let (body, _) = capture_warnings(
                move |url| {
                    Arc::new(
                        create_google(GoogleProviderSettings {
                            base_url: Some(url),
                            api_key: Some("test-key".into()),
                            ..GoogleProviderSettings::default()
                        })
                        .unwrap()
                        .chat(model),
                    )
                },
                prepared.options,
            )
            .await;
            (Some(body), None)
        }
        Err(error) => (None, Some(error.to_string())),
    }
}

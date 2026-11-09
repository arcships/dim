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

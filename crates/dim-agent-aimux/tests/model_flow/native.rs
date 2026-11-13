use super::*;
use axum::http::Uri;
use dim_agent_runtime::testing::chain_events;
use rutis::host::CtxHost;

const IMAGE: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jD1sAAAAASUVORK5CYII=";

fn bedrock_frame(name: &str, payload: Value) -> Vec<u8> {
    aimux_providers::bedrock::event_stream_decoder::encode_message(
        "event",
        name,
        &payload.to_string(),
    )
}

#[tokio::test]
async fn bedrock_native_auth_stream_and_continuation_are_first_party() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let credentials: AwsCredentials = serde_json::from_value(json!({
        "accessKeyId":"access-contract", "secretAccessKey":"secret-contract"
    }))
    .unwrap();
    assert_eq!(credentials.session_token, None);
    assert_eq!(
        serde_json::to_value(&credentials).unwrap(),
        json!({"accessKeyId":"access-contract","secretAccessKey":"secret-contract"})
    );
    assert!(
        serde_json::from_value::<AwsCredentials>(json!({
            "accessKeyId":"access", "secretAccessKey":"secret", "sessionToken":null
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AwsCredentials>(json!({
            "accessKeyId":"access", "secretAccessKey":"secret", "region":"us-east-1"
        }))
        .is_err()
    );

    for auth in [
        BedrockAuth::Bearer("bearer-asymmetric".into()),
        BedrockAuth::SigV4 {
            credentials: AwsCredentials {
                access_key_id: "access-asymmetric".into(),
                secret_access_key: "secret-asymmetric".into(),
                session_token: Some("session-asymmetric".into()),
            },
            region: "us-west-2".into(),
        },
    ] {
        let server = MockServer::start().await;
        let body = [
            bedrock_frame("messageStart", json!({"role":"assistant"})),
            bedrock_frame("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"text":"answer-A"}})),
            bedrock_frame("contentBlockStop", json!({"contentBlockIndex":0})),
            bedrock_frame("contentBlockDelta", json!({"contentBlockIndex":1,"delta":{"reasoningContent":{"text":"signed-thought"}}})),
            bedrock_frame("contentBlockDelta", json!({"contentBlockIndex":1,"delta":{"reasoningContent":{"signature":"signature-A"}}})),
            bedrock_frame("contentBlockStop", json!({"contentBlockIndex":1})),
            bedrock_frame("contentBlockDelta", json!({"contentBlockIndex":2,"delta":{"reasoningContent":{"redactedContent":"cmVkYWN0ZWQtQg=="}}})),
            bedrock_frame("contentBlockStop", json!({"contentBlockIndex":2})),
            bedrock_frame("contentBlockDelta", json!({"contentBlockIndex":3,"delta":{"reasoningContent":{"text":"unsigned-C"}}})),
            bedrock_frame("contentBlockStop", json!({"contentBlockIndex":3})),
            bedrock_frame("contentBlockStart", json!({"contentBlockIndex":4,"start":{"toolUse":{"toolUseId":"call-bedrock","name":"echo"}}})),
            bedrock_frame("contentBlockDelta", json!({"contentBlockIndex":4,"delta":{"toolUse":{"input":"{\"text\":\"Tokyo-A\"}"}}})),
            bedrock_frame("contentBlockStop", json!({"contentBlockIndex":4})),
            bedrock_frame("messageStop", json!({"stopReason":"tool_use"})),
            bedrock_frame("metadata", json!({"usage":{"inputTokens":19,"outputTokens":11,"totalTokens":30}})),
        ].concat();
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/vnd.amazon.eventstream")
                    .set_body_bytes(body),
            )
            .mount(&server)
            .await;
        let root = rutis::Ctx::root().unwrap();
        root.plugin(AimuxPlugin).await.unwrap();
        let host = root.get::<ModelHost>().unwrap();
        let connection = || {
            ModelConnection::Bedrock(BedrockModelConnection {
                base_url: server.uri(),
                network: Default::default(),
                headers: [("X-Tenant".into(), "tenant-A".into())].into(),
                auth: auth.clone(),
            })
        };
        let mut request = ModelRequest::new(
            "bedrock",
            "arn:aws:bedrock:us-west-2:123:inference-profile/model-A",
            vec![Message {
                role: Role::User,
                content: MessageContent::Parts(vec![
                    MessagePart::Text {
                        text: "question-A".into(),
                        provider_metadata: None,
                        start_time: None,
                        end_time: None,
                    },
                    MessagePart::Image {
                        source: ImageSource {
                            source_type: ImageSourceType::Base64,
                            media_type: "image/png".into(),
                            data: Some(IMAGE.into()),
                            url: None,
                        },
                    },
                ]),
            }],
        );
        request.tools = Some(MockTools.definitions().into());
        request.config = Some(ModelConfig {
            max_tokens: Some(4096.0),
            temperature: Some(0.25),
            top_p: Some(0.75),
            stop_sequences: Some(vec!["STOP-A".into()]),
            reasoning: Some(ProviderReasoningConfig {
                budget_tokens: Some(1024.0),
                ..Default::default()
            }),
            ..Default::default()
        });
        let mut stream = host
            .0
            .invoke(
                Arc::new(request.clone()),
                connection(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut content = None;
        let mut stop = None;
        while let Some(chunk) = stream.next().await {
            match chunk.unwrap() {
                ModelChunk::Content(parts) => content = Some(parts),
                ModelChunk::MessageStop { reason, .. } => stop = Some(reason),
                _ => {}
            }
        }
        assert_eq!(stop, Some(ModelStopReason::ToolUse));
        let content = content.unwrap();
        assert!(
            matches!(&content[1], MessagePart::Thinking { thinking, provider_metadata: Some(ContentMetadata::Bedrock { signature: Some(signature) }), .. } if thinking == "signed-thought" && signature == "signature-A")
        );
        assert!(
            matches!(&content[2], MessagePart::Thinking { provider_metadata: Some(ContentMetadata::BedrockRedacted { data }), .. } if data == "cmVkYWN0ZWQtQg==")
        );
        assert!(
            matches!(&content[3], MessagePart::Thinking { thinking, provider_metadata: Some(ContentMetadata::Bedrock { signature: None }), .. } if thinking == "unsigned-C")
        );
        let replay: Vec<MessagePart> =
            serde_json::from_slice(&serde_json::to_vec(&content).unwrap()).unwrap();
        request.messages = vec![
            Arc::new(Message::text(Role::User, "question-A")),
            Arc::new(Message {
                role: Role::Assistant,
                content: MessageContent::Parts(replay),
            }),
            Arc::new(Message {
                role: Role::ToolResult,
                content: MessageContent::Parts(vec![MessagePart::ToolResult {
                    tool_use_id: "call-bedrock".into(),
                    content: "tool-result-D".into(),
                    structured_content: None,
                    is_error: None,
                }]),
            }),
        ];
        let mut replay_stream = host
            .0
            .invoke(
                Arc::new(request.clone()),
                connection(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        while let Some(chunk) = replay_stream.next().await {
            chunk.unwrap();
        }
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 2);
        let first: Value = serde_json::from_slice(&sent[0].body).unwrap();
        assert_eq!(
            first["messages"][0]["content"],
            json!([
                {"text":"question-A"},
                {"image":{"format":"png","source":{"bytes":IMAGE}}}
            ])
        );
        assert_eq!(
            first["inferenceConfig"],
            json!({"maxTokens":4096,"temperature":0.25,"topP":0.75,"stopSequences":["STOP-A"]})
        );
        assert_eq!(
            first["additionalModelRequestFields"]["thinking"],
            json!({"type":"enabled","budget_tokens":1024})
        );
        let second: Value = serde_json::from_slice(&sent[1].body).unwrap();
        assert_eq!(
            second["messages"][1]["content"],
            json!([
                {"text":"answer-A"},
                {"reasoningContent":{"reasoningText":{"text":"signed-thought","signature":"signature-A"}}},
                {"reasoningContent":{"redactedContent":"cmVkYWN0ZWQtQg=="}},
                {"toolUse":{"toolUseId":"call-bedrock","name":"echo","input":{"text":"Tokyo-A"}}}
            ])
        );
        assert_eq!(sent[1].headers["x-tenant"], "tenant-A");
        match auth {
            BedrockAuth::Bearer(_) => {
                assert_eq!(sent[1].headers["authorization"], "Bearer bearer-asymmetric")
            }
            BedrockAuth::SigV4 { .. } => {
                let authorization = sent[1].headers["authorization"].to_str().unwrap();
                assert!(authorization.contains("Credential=access-asymmetric/"));
                // aws4fetch 1.0.20, the signer of the AI SDK's AWS providers,
                // sends `content-type` but leaves it out of `SignedHeaders`;
                // the tag's signer carries that list, not a patch.
                let signed_headers = authorization
                    .split_once("SignedHeaders=")
                    .and_then(|(_, rest)| rest.split(',').next())
                    .expect("the Authorization header names its signed headers");
                assert!(
                    !signed_headers
                        .split(';')
                        .any(|name| name.eq_ignore_ascii_case("content-type")),
                    "content-type must not be signed: {signed_headers}"
                );
                assert_eq!(
                    sent[1].headers["x-amz-security-token"],
                    "session-asymmetric"
                );
            }
        }
        let cross = ModelConnection::Http(HttpModelConnection {
            protocol: ModelProtocol::Anthropic,
            base_url: server.uri(),
            network: Default::default(),
            headers: Default::default(),
            bearer: None,
            api_key: None,
            preset: None,
        });
        // Stored Bedrock continuation data never reaches another protocol:
        // the request is built without it instead of failing.
        let mut foreign = host
            .0
            .invoke(Arc::new(request), cross, CancellationToken::new())
            .await
            .expect("another protocol's metadata must not fail the request");
        while let Some(chunk) = foreign.next().await {
            // The mock answers in Bedrock's framing; only the request matters.
            let _ = chunk;
        }
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 3);
        let foreign = String::from_utf8(sent[2].body.clone()).unwrap();
        assert!(!foreign.contains("signature-A"), "{foreign}");
        assert!(!foreign.contains("redactedContent"), "{foreign}");
        for config in [
            json!({"topK":7}),
            json!({"reasoning":{"effort":"high"}}),
            json!({"reasoning":{"budgetTokens":1.5}}),
            json!({"providerOptions":{"openai":{"serviceTier":"priority"}}}),
        ] {
            let mut invalid = ModelRequest::new(
                "bedrock",
                "model-invalid",
                vec![Message::text(Role::User, "invalid")],
            );
            invalid.config = Some(serde_json::from_value(config).unwrap());
            assert!(
                host.0
                    .invoke(Arc::new(invalid), connection(), CancellationToken::new())
                    .await
                    .is_err()
            );
        }
        let mut invalid = ModelRequest::new(
            "bedrock",
            "model-invalid",
            vec![Message {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![MessagePart::Text {
                    text: "wrong-kind".into(),
                    start_time: None,
                    end_time: None,
                    provider_metadata: Some(ContentMetadata::Bedrock {
                        signature: Some("signature".into()),
                    }),
                }]),
            }],
        );
        invalid.config = Some(ModelConfig::default());
        assert!(
            host.0
                .invoke(Arc::new(invalid), connection(), CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        let url_image = ModelRequest::new(
            "bedrock",
            "model-url-image",
            vec![Message {
                role: Role::User,
                content: MessageContent::Parts(vec![
                    MessagePart::Text {
                        text: "Describe the attached image".into(),
                        provider_metadata: None,
                        start_time: None,
                        end_time: None,
                    },
                    MessagePart::Image {
                        source: ImageSource {
                            source_type: ImageSourceType::Url,
                            media_type: "image/png".into(),
                            data: None,
                            url: Some("https://example.invalid/private-image.png".into()),
                        },
                    },
                ]),
            }],
        );
        assert!(matches!(
            host.0
                .invoke(Arc::new(url_image), connection(), CancellationToken::new())
                .await,
            Err(HostError::Failed {
                kind: HostErrorKind::Unsupported,
                ..
            })
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        let cancellation = CancellationToken::new();
        let mut cancelled = host
            .0
            .invoke(
                Arc::new(ModelRequest::new(
                    "bedrock",
                    "model-cancel",
                    vec![Message::text(Role::User, "cancel-E")],
                )),
                connection(),
                cancellation.clone(),
            )
            .await
            .unwrap();
        assert!(matches!(
            cancelled.next().await,
            Some(Ok(ModelChunk::MessageStart { .. }))
        ));
        cancellation.cancel();
        assert!(matches!(
            cancelled.next().await,
            Some(Err(HostError::Cancelled(_)))
        ));
        let mut held = host
            .0
            .invoke(
                Arc::new(ModelRequest::new(
                    "bedrock",
                    "model-held",
                    vec![Message::text(Role::User, "held-F")],
                )),
                connection(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        root.shutdown().await.unwrap();
        assert!(matches!(
            held.next().await,
            Some(Err(HostError::Failed {
                kind: HostErrorKind::OwnerClosed,
                ..
            }))
        ));
    }
}

#[tokio::test]
async fn bedrock_unreadable_tool_delta_fails_the_turn_without_a_call() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let server = MockServer::start().await;
    let body = [
        bedrock_frame("messageStart", json!({"role":"assistant"})),
        bedrock_frame(
            "contentBlockStart",
            json!({"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"call-array","name":"echo"}}}),
        ),
        bedrock_frame(
            "contentBlockDelta",
            json!({"contentBlockIndex":0,"delta":{"toolUse":{"input":["unreadable"]}}}),
        ),
        bedrock_frame("contentBlockStop", json!({"contentBlockIndex":0})),
        bedrock_frame("messageStop", json!({"stopReason":"tool_use"})),
        bedrock_frame(
            "metadata",
            json!({"usage":{"inputTokens":1,"outputTokens":1,"totalTokens":2}}),
        ),
    ]
    .concat();
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(body),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let mut request = ModelRequest::new(
        "bedrock",
        "anthropic.model",
        vec![Message::text(Role::User, "run")],
    );
    request.tools = Some(MockTools.definitions().into());
    let connection = ModelConnection::Bedrock(BedrockModelConnection {
        base_url: server.uri(),
        network: Default::default(),
        headers: Default::default(),
        auth: BedrockAuth::Bearer("bearer".into()),
    });
    let mut output = host
        .0
        .invoke(Arc::new(request), connection, CancellationToken::new())
        .await
        .unwrap();
    let mut tool_use = false;
    let mut failure = None;
    while let Some(chunk) = output.next().await {
        match chunk {
            Ok(ModelChunk::Content(parts)) => {
                tool_use |= parts
                    .iter()
                    .any(|part| matches!(part, MessagePart::ToolUse { .. }));
            }
            Ok(_) => {}
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let failure = failure.expect("the unreadable delta must fail the turn");
    assert!(matches!(failure, HostError::Failed { .. }), "{failure:?}");
    assert!(!tool_use, "no tool call may come from an unreadable delta");
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn bedrock_finishes_at_metadata_while_the_body_stays_open() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    for cancel in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (proceed, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let length = String::from_utf8_lossy(&request[..end])
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.amazon.eventstream\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            let first = [
                bedrock_frame("messageStart", json!({"role":"assistant"})),
                bedrock_frame(
                    "contentBlockDelta",
                    json!({"contentBlockIndex":0,"delta":{"text":"early"}}),
                ),
            ]
            .concat();
            // A chunk ending exactly at a frame prelude exercises internal
            // decoder state.
            for bytes in [&first[..12], &first[12..]] {
                socket
                    .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(bytes).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            ready.await.unwrap();
            if !cancel {
                let final_bytes = [
                    bedrock_frame("contentBlockStop", json!({"contentBlockIndex":0})),
                    bedrock_frame("messageStop", json!({"stopReason":"end_turn"})),
                    bedrock_frame(
                        "metadata",
                        json!({"usage":{"inputTokens":17,"outputTokens":9,"totalTokens":26}}),
                    ),
                ]
                .concat();
                socket
                    .write_all(format!("{:x}\r\n", final_bytes.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(&final_bytes).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            std::future::pending::<()>().await;
        });
        let root = rutis::Ctx::root().unwrap();
        root.plugin(AimuxPlugin).await.unwrap();
        let host = root.get::<ModelHost>().unwrap();
        let request = ModelRequest::new(
            "bedrock",
            "anthropic.model",
            vec![Message::text(Role::User, "hi")],
        );
        let connection = ModelConnection::Bedrock(BedrockModelConnection {
            base_url: base,
            network: Default::default(),
            headers: Default::default(),
            auth: BedrockAuth::Bearer("bearer".into()),
        });
        let cancel_token = CancellationToken::new();
        let mut output = host
            .0
            .invoke(Arc::new(request), connection, cancel_token.clone())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if matches!(output.next().await.unwrap().unwrap(), ModelChunk::Text(text) if text == "early") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        if cancel {
            cancel_token.cancel();
        }
        proceed.send(()).unwrap();
        let mut usage = None;
        let mut stopped = false;
        let mut failure = false;
        if cancel {
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(2), output.next()).await {
                    Ok(Some(Ok(ModelChunk::MessageStop { .. }))) => {
                        stopped = true;
                        break;
                    }
                    Ok(Some(Ok(_))) => {}
                    Ok(Some(Err(_))) => {
                        failure = true;
                        break;
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            assert!(failure, "an abort must end the flow with an error");
            assert!(!stopped, "an abort must not deliver MessageStop");
        } else {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while let Some(chunk) = output.next().await {
                    match chunk.unwrap() {
                        ModelChunk::Usage(value) => usage = Some(value),
                        ModelChunk::MessageStop { .. } => {
                            stopped = true;
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("MessageStop must arrive while the body stays open");
            assert!(stopped, "the terminal event must deliver MessageStop");
            let usage = usage.expect("usage must arrive before the stop");
            assert_eq!((usage.prompt_tokens, usage.completion_tokens), (17.0, 9.0));
        }
        server.abort();
        let _ = server.await;
        root.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn xai_native_continuation_roundtrips_and_owner_releases_streams() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let server = MockServer::start().await;
    let frames = [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs-native"}}),
        json!({"type":"response.reasoning_summary_text.delta","item_id":"rs-native","summary_index":0,"delta":"first "}),
        json!({"type":"response.reasoning_summary_part.added","item_id":"rs-native","summary_index":0}),
        json!({"type":"response.reasoning_summary_text.delta","item_id":"rs-native","summary_index":0,"delta":"thought"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs-native","encrypted_content":"opaque-native"}}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc-native","call_id":"call-native","name":"echo"}}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"text\":\"result\"}"}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc-native","call_id":"call-native","name":"echo","arguments":"{\"text\":\"result\"}"}}),
        json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":9,"output_tokens":5}}}),
    ].into_iter().map(|frame| format!("data: {frame}\n\n")).collect::<String>();
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(frames),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"visible\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
        .mount(&server).await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let connection = |protocol| {
        ModelConnection::Http(HttpModelConnection {
            protocol,
            base_url: format!("{}?tenant=native", server.uri()),
            network: Default::default(),
            headers: std::collections::HashMap::from([(
                "content-type".into(),
                "application/json".into(),
            )]),
            bearer: Some("fixture-key".into()),
            api_key: None,
            preset: None,
        })
    };
    let mut request = ModelRequest::new("xai", "alias", vec![Message::text(Role::User, "start")]);
    request.tools = Some(MockTools.definitions().into());
    let mut output = host
        .0
        .invoke(
            Arc::new(request.clone()),
            connection(ModelProtocol::XaiResponses),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut parts = None;
    let mut stop = None;
    while let Some(chunk) = output.next().await {
        match chunk.unwrap() {
            ModelChunk::Content(value) => parts = Some(value),
            ModelChunk::MessageStop { reason, .. } => stop = Some(reason),
            _ => {}
        }
    }
    assert_eq!(stop, Some(ModelStopReason::ToolUse));
    let parts = parts.expect("native content must be emitted");
    assert!(
        matches!(&parts[0], MessagePart::Thinking { thinking, provider_metadata: Some(ContentMetadata::XaiResponses { item_id, encrypted_content: Some(encrypted) }), .. } if thinking == "first thought" && item_id == "rs-native" && encrypted == "opaque-native")
    );
    let serialized = serde_json::to_vec(&parts).unwrap();
    let replay: Vec<MessagePart> = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(parts, replay);
    request.messages = vec![
        Message::text(Role::User, "start"),
        Message {
            role: Role::Assistant,
            content: MessageContent::Parts(replay),
        },
        Message {
            role: Role::ToolResult,
            content: MessageContent::Parts(vec![MessagePart::ToolResult {
                tool_use_id: "call-native".into(),
                content: "result".into(),
                is_error: None,
                structured_content: None,
            }]),
        },
    ]
    .into_iter()
    .map(Arc::new)
    .collect();
    let mut output = host
        .0
        .invoke(
            Arc::new(request.clone()),
            connection(ModelProtocol::XaiResponses),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    while let Some(chunk) = output.next().await {
        chunk.unwrap();
    }
    let sent = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&sent[1].body).unwrap();
    assert_eq!(
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["type"] == "reasoning")
            .unwrap(),
        &json!({"type":"reasoning","id":"rs-native","status":"completed","summary":[{"type":"summary_text","text":"first thought"}],"encrypted_content":"opaque-native"})
    );
    assert_eq!(
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["type"] == "function_call")
            .unwrap()["id"],
        "fc-native"
    );
    assert_eq!(body["store"], false);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    // xAI continuation data does not reach the OpenAI Responses protocol: the
    // request is built without the reasoning item instead of failing.
    let mut foreign = host
        .0
        .invoke(
            Arc::new(request),
            connection(ModelProtocol::OpenaiResponses),
            CancellationToken::new(),
        )
        .await
        .expect("another protocol's metadata must not fail the request");
    while let Some(chunk) = foreign.next().await {
        // The mock answers in xAI's framing; only the request matters.
        let _ = chunk;
    }
    let sent = server.received_requests().await.unwrap();
    assert_eq!(sent.len(), 3);
    let foreign = String::from_utf8(sent[2].body.clone()).unwrap();
    assert!(!foreign.contains("rs-native"), "{foreign}");
    assert!(!foreign.contains("opaque-native"), "{foreign}");
    for protocol in [ModelProtocol::Xai, ModelProtocol::XaiResponses] {
        for config in [
            ModelConfig {
                top_k: Some(7.),
                ..Default::default()
            },
            ModelConfig {
                provider_options: Some(ProviderOptions {
                    openai: Some(OpenAiProviderOptions {
                        service_tier: Some("priority".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ] {
            let mut request =
                ModelRequest::new("xai", "alias", vec![Message::text(Role::User, "start")]);
            request.config = Some(config);
            assert!(
                host.0
                    .invoke(
                        Arc::new(request),
                        connection(protocol),
                        CancellationToken::new()
                    )
                    .await
                    .is_err()
            );
        }
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    let mut held = Vec::new();
    for protocol in [ModelProtocol::Xai, ModelProtocol::XaiResponses] {
        let mut request =
            ModelRequest::new("xai", "alias", vec![Message::text(Role::User, "start")]);
        request.tools = Some(MockTools.definitions().into());
        let request = Arc::new(request);
        let cancel = CancellationToken::new();
        let mut output = host
            .0
            .invoke(request.clone(), connection(protocol), cancel.clone())
            .await
            .unwrap();
        assert!(matches!(
            output.next().await,
            Some(Ok(ModelChunk::MessageStart { .. }))
        ));
        cancel.cancel();
        assert!(matches!(
            output.next().await,
            Some(Err(HostError::Cancelled(_)))
        ));
        held.push(
            host.0
                .invoke(request, connection(protocol), CancellationToken::new())
                .await
                .unwrap(),
        );
    }
    for request in server.received_requests().await.unwrap() {
        assert_eq!(request.url.query(), Some("tenant=native"));
        assert_eq!(request.headers["authorization"], "Bearer fixture-key");
        assert_eq!(request.headers.get_all("content-type").iter().count(), 1);
    }
    root.shutdown().await.unwrap();
    for mut stream in held {
        assert!(matches!(
            stream.next().await,
            Some(Err(HostError::Failed {
                kind: HostErrorKind::OwnerClosed,
                ..
            }))
        ));
    }
}

#[tokio::test]
async fn xai_responses_content_parts_keep_their_blocks() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let server = MockServer::start().await;
    let frames = [
        json!({"type":"response.created","response":{"id":"resp-parts","created_at":1,"model":"test-model"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_1"}}),
        json!({"type":"response.output_text.delta","item_id":"msg_1","content_index":0,"delta":"before"}),
        json!({"type":"response.output_text.delta","item_id":"msg_1","content_index":1,"delta":"after"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg_1","content":[
            {"type":"output_text","text":"before"},
            {"type":"output_text","text":"after"},
            {"type":"output_text","text":"only-in-done"}
        ]}}),
        json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":3,"output_tokens":3}}}),
    ]
    .into_iter()
    .map(|frame| format!("data: {frame}\n\n"))
    .collect::<String>();
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(frames),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let request = ModelRequest::new("xai", "alias", vec![Message::text(Role::User, "start")]);
    let mut output = host
        .0
        .invoke(
            Arc::new(request),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::XaiResponses,
                base_url: format!("{}?tenant=native", server.uri()),
                network: Default::default(),
                headers: std::collections::HashMap::new(),
                bearer: Some("fixture-key".into()),
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut parts = None;
    while let Some(chunk) = output.next().await {
        if let ModelChunk::Content(value) = chunk.unwrap() {
            parts = Some(value);
        }
    }
    let parts = parts.expect("native content must be emitted");
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["before", "after", "only-in-done"]);
    let request = ModelRequest::new(
        "xai",
        "alias",
        vec![
            Message::text(Role::User, "start"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![
                    MessagePart::Text {
                        text: "before".into(),
                        start_time: None,
                        end_time: None,
                        provider_metadata: Some(ContentMetadata::XaiResponses {
                            item_id: "msg_1".into(),
                            encrypted_content: None,
                        }),
                    },
                    MessagePart::Text {
                        text: "after".into(),
                        start_time: None,
                        end_time: None,
                        provider_metadata: Some(ContentMetadata::XaiResponses {
                            item_id: "msg_1".into(),
                            encrypted_content: None,
                        }),
                    },
                ]),
            },
        ],
    );
    let mut output = host
        .0
        .invoke(
            Arc::new(request),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::XaiResponses,
                base_url: format!("{}?tenant=native", server.uri()),
                network: Default::default(),
                headers: std::collections::HashMap::new(),
                bearer: Some("fixture-key".into()),
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    while let Some(chunk) = output.next().await {
        chunk.unwrap();
    }
    let sent = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&sent[1].body).unwrap();
    let messages: Vec<&Value> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|part| part["role"] == "assistant")
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["id"], "msg_1");
    // xAI replays one message item's adjacent text parts as one item with
    // their joined text; the stream above still yields separate blocks.
    assert_eq!(messages[0]["content"], json!("beforeafter"));
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_preferences_reach_chat_and_responses_for_model_aliases() {
    for (kind, protocol) in [
        ("openai-chat", ModelProtocol::OpenaiChat),
        ("openai-chat", ModelProtocol::OpenaiCompatible),
        ("openai-responses", ModelProtocol::OpenaiResponses),
    ] {
        let server = NativeServer::start(kind).await;
        let root = rutis::Ctx::root().unwrap();
        root.plugin(AimuxPlugin).await.unwrap();
        let host = root.get::<ModelHost>().unwrap();
        for (tier, effort) in [
            (Some("priority"), Some("high")),
            (Some("flex"), Some("none")),
            (Some("future-tier"), Some("low")),
            (None, None),
        ] {
            let mut request = ModelRequest::new(
                "tenant",
                "tenant-alias",
                vec![Message::text(Role::User, "hello")],
            );
            let mut options = ProviderOptions::default();
            if protocol == ModelProtocol::OpenaiCompatible {
                options.openai_compatible = Some(OpenAiCompatibleProviderOptions {
                    service_tier: tier.map(str::to_owned),
                    ..Default::default()
                });
            } else {
                options.openai = Some(OpenAiProviderOptions {
                    service_tier: tier.map(str::to_owned),
                    ..Default::default()
                });
            }
            request.config = Some(ModelConfig {
                provider_options: Some(options),
                reasoning: effort.map(|effort| ProviderReasoningConfig {
                    effort: Some(effort.into()),
                    ..Default::default()
                }),
                ..Default::default()
            });
            let connection = ModelConnection::Http(HttpModelConnection {
                protocol,
                base_url: server.url.clone(),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            });
            let mut stream = host
                .0
                .invoke(Arc::new(request), connection, CancellationToken::new())
                .await
                .unwrap();
            let mut stopped = false;
            while let Some(chunk) = stream.next().await {
                stopped |= matches!(chunk.unwrap(), ModelChunk::MessageStop { .. });
            }
            assert!(stopped);
            let requests = server.endpoint.requests.lock().unwrap();
            let body = &requests.last().unwrap().2;
            assert_eq!(body["model"], "tenant-alias");
            assert_eq!(body.get("service_tier").and_then(Value::as_str), tier);
            let sent_effort = if protocol == ModelProtocol::OpenaiResponses {
                body["reasoning"].get("effort")
            } else {
                body.get("reasoning_effort")
            };
            assert_eq!(sent_effort.and_then(Value::as_str), effort);
        }
        root.shutdown().await.unwrap();
        server.close().await;
    }
}

#[tokio::test]
async fn codex_request_profile_is_isolated_to_responses() {
    let server = NativeServer::start("openai-responses").await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let request = || {
        let mut request = ModelRequest::new(
            "openai-codex",
            "gpt-5-codex",
            vec![
                Message::text(Role::User, "history"),
                Message::text(Role::Assistant, "answer"),
                Message::text(Role::User, "current"),
            ],
        );
        request.system = Some(Arc::new(SystemPrompt::Text("system instructions".into())));
        request.tools = Some(MockTools.definitions().into());
        request.config = Some(ModelConfig {
            max_tokens: Some(2048.0),
            reasoning: Some(ProviderReasoningConfig {
                effort: Some("high".into()),
                ..Default::default()
            }),
            provider_options: Some(ProviderOptions {
                openai: Some(OpenAiProviderOptions {
                    service_tier: Some("priority".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
        Arc::new(request)
    };
    let headers = std::collections::HashMap::from([
        ("ChatGPT-Account-ID".into(), "account-1".into()),
        ("OAI-Product-Sku".into(), "codex".into()),
        ("originator".into(), "codex-tui".into()),
        ("User-Agent".into(), "dimagent/0.144.1".into()),
    ]);
    let connection = |protocol, preset: Option<&str>| {
        ModelConnection::Http(HttpModelConnection {
            protocol,
            base_url: server.url.clone(),
            network: Default::default(),
            headers: headers.clone(),
            bearer: Some("oauth-secret".into()),
            api_key: None,
            preset: preset.map(str::to_owned),
        })
    };

    let mut stream = host
        .0
        .invoke(
            request(),
            connection(ModelProtocol::OpenaiResponses, Some("openai-codex")),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    let requests = server.endpoint.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let (uri, sent_headers, body) = &requests[0];
    assert_eq!(uri.path(), "/v1/responses");
    assert_eq!(sent_headers["authorization"], "Bearer oauth-secret");
    assert_eq!(sent_headers["chatgpt-account-id"], "account-1");
    assert_eq!(sent_headers["oai-product-sku"], "codex");
    assert_eq!(sent_headers["originator"], "codex-tui");
    assert_eq!(
        sent_headers["user-agent"],
        "dimagent/0.144.1 ai-sdk-provider-utils/5.0.53"
    );
    assert_eq!(body["store"], false);
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(body["reasoning"]["effort"], "high");
    assert!(body["input"].to_string().contains("system instructions"));
    assert_eq!(body["tools"][0]["type"], "function");
    assert!(body["input"].as_array().unwrap().len() >= 3);

    let before = server.endpoint.requests.lock().unwrap().len();
    let error = host
        .0
        .invoke(
            request(),
            connection(ModelProtocol::OpenaiChat, Some("openai-codex")),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error,
        HostError::Failed {
            kind: HostErrorKind::InvalidRequest,
            ..
        }
    ));
    assert_eq!(server.endpoint.requests.lock().unwrap().len(), before);

    let mut stream = host
        .0
        .invoke(
            request(),
            connection(ModelProtocol::OpenaiResponses, None),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    {
        let requests = server.endpoint.requests.lock().unwrap();
        assert_eq!(requests.last().unwrap().2["max_output_tokens"], 2048);
    }
    root.shutdown().await.unwrap();
    server.close().await;
}

#[tokio::test]
async fn openai_chat_protocols_send_reasoning_models_the_rules_they_accept() {
    let server = NativeServer::start("openai-chat").await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    for protocol in [ModelProtocol::OpenaiChat, ModelProtocol::OpenaiCompatible] {
        for model_id in ["gpt-4o", "gpt-5", "o3"] {
            let mut request =
                ModelRequest::new("local", model_id, vec![Message::text(Role::User, "hi")]);
            request.system = Some(Arc::new(SystemPrompt::Text("system instructions".into())));
            request.config = Some(ModelConfig {
                max_tokens: Some(4096.0),
                temperature: Some(0.25),
                reasoning: Some(ProviderReasoningConfig {
                    effort: Some("high".into()),
                    ..Default::default()
                }),
                ..Default::default()
            });
            let connection = ModelConnection::Http(HttpModelConnection {
                protocol,
                base_url: server.url.clone(),
                network: Default::default(),
                headers: Default::default(),
                bearer: Some("native-secret".into()),
                api_key: None,
                preset: None,
            });
            let mut stream = host
                .0
                .invoke(Arc::new(request), connection, CancellationToken::new())
                .await
                .unwrap();
            while let Some(chunk) = stream.next().await {
                chunk.unwrap();
            }
        }
    }
    // Two turns on gpt-5 over openai-chat: the second request replays the
    // assistant's reasoning text with its tool call.
    let mut first = ModelRequest::new("local", "gpt-5", vec![Message::text(Role::User, "hi")]);
    first.system = Some(Arc::new(SystemPrompt::Text("system instructions".into())));
    first.config = Some(ModelConfig {
        max_tokens: Some(4096.0),
        reasoning: Some(ProviderReasoningConfig {
            effort: Some("high".into()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let connection = ModelConnection::Http(HttpModelConnection {
        protocol: ModelProtocol::OpenaiChat,
        base_url: server.url.clone(),
        network: Default::default(),
        headers: Default::default(),
        bearer: Some("native-secret".into()),
        api_key: None,
        preset: None,
    });
    let mut stream = host
        .0
        .invoke(Arc::new(first), connection, CancellationToken::new())
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    // The second request carries the history from the first.
    let history: ModelRequest = serde_json::from_value(json!({
        "providerId": "local",
        "modelId": "gpt-5",
        "system": [{"type": "text", "text": "system instructions"}],
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan"},
                {"type": "text", "text": "searching"},
                {"type": "tool_use", "id": "call-1", "name": "search", "input": {"query": "recent"}}
            ]},
            {"role": "tool_result", "content": [{"type": "tool_result", "tool_use_id": "call-1", "content": [{"type": "text", "text": "done"}]}]},
            {"role": "user", "content": "continue"}
        ],
        "config": {"maxTokens": 4096, "reasoning": {"effort": "high"}}
    }))
    .unwrap();
    let connection = ModelConnection::Http(HttpModelConnection {
        protocol: ModelProtocol::OpenaiChat,
        base_url: server.url.clone(),
        network: Default::default(),
        headers: Default::default(),
        bearer: Some("native-secret".into()),
        api_key: None,
        preset: None,
    });
    let mut stream = host
        .0
        .invoke(Arc::new(history), connection, CancellationToken::new())
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    let bodies: Vec<Value> = server
        .endpoint
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(uri, _, body)| {
            assert_eq!(uri.path(), "/v1/chat/completions");
            body.clone()
        })
        .collect();
    assert_eq!(bodies.len(), 8);
    // A reasoning model takes the token key, the system role and the sampling
    // parameters the OpenAI API accepts; every other model keeps `max_tokens`,
    // `system` and the sampling values.
    for (index, body) in bodies[..6].iter().enumerate() {
        let model_id = ["gpt-4o", "gpt-5", "o3"][index % 3];
        let protocol = if index < 3 {
            "openai-chat"
        } else {
            "openai-compatible"
        };
        let expected = if index % 3 == 0 {
            json!({
                "model": model_id,
                "max_tokens": 4096,
                "temperature": 0.25,
                "messages": [
                    {"role": "system", "content": "system instructions"},
                    {"role": "user", "content": "hi"}
                ],
                "stream": true,
                "stream_options": {"include_usage": true},
                "reasoning_effort": "high"
            })
        } else {
            json!({
                "model": model_id,
                "max_completion_tokens": 4096,
                "messages": [
                    {"role": "developer", "content": "system instructions"},
                    {"role": "user", "content": "hi"}
                ],
                "stream": true,
                "stream_options": {"include_usage": true},
                "reasoning_effort": "high"
            })
        };
        assert_eq!(body, &expected, "{protocol} body {index}");
    }
    // The first turn of the pair sends the reasoning-model rules.
    assert_eq!(bodies[6]["model"], "gpt-5");
    assert_eq!(bodies[6]["max_completion_tokens"], 4096);
    assert_eq!(
        bodies[6]["messages"][0],
        json!({"role": "developer", "content": "system instructions"})
    );
    // The second turn keeps the reasoning-model rules and replays the
    // assistant's reasoning text with its tool call.
    let body = &bodies[7];
    assert_eq!(body["model"], "gpt-5");
    assert_eq!(body["max_completion_tokens"], 4096);
    assert!(body.get("max_tokens").is_none(), "{body}");
    assert_eq!(
        body["messages"][0],
        json!({"role": "developer", "content": "system instructions"})
    );
    assert_eq!(body["messages"][2]["reasoning_content"], "plan", "{body}");
    assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call-1");
}

#[derive(Clone)]
struct NativeEndpoint {
    kind: &'static str,
    requests: Arc<Mutex<Vec<(Uri, HeaderMap, Value)>>>,
}
struct NativeServer {
    endpoint: NativeEndpoint,
    url: String,
    cancel: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}
impl NativeServer {
    async fn start(kind: &'static str) -> Self {
        let endpoint = NativeEndpoint {
            kind,
            requests: Default::default(),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(respond_native))
            .route("/v1/responses", post(respond_native))
            .route("/v1/messages", post(respond_native))
            .route(
                "/v1/models/test-model:streamGenerateContent",
                post(respond_native),
            )
            .with_state(endpoint.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1?tenant=sandbox",
            listener.local_addr().unwrap()
        );
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let join = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .unwrap();
        });
        Self {
            endpoint,
            url,
            cancel,
            join,
        }
    }
    fn model_id(&self) -> &'static str {
        if self.endpoint.kind == "openai-responses" {
            "gpt-5-mini"
        } else {
            "test-model"
        }
    }
    fn session_config(&self) -> SessionConfig {
        let mut config = super::session_config();
        config.model.model_id = self.model_id().into();
        config
    }
    fn models(&self) -> Arc<AimuxModels> {
        let header = match self.endpoint.kind {
            "anthropic" => "x-api-key",
            "gemini" => "x-goog-api-key",
            _ => "Authorization",
        };
        let credential = if header == "Authorization" {
            "Bearer native-secret"
        } else {
            "native-secret"
        };
        let mut value = json!({"defaultModel":{"providerId":"local","modelId":self.model_id()},"providers":[{"id":"local","kind":self.endpoint.kind,"baseUrl":self.url,"headers":{header:credential},"models":[{"id":self.model_id(),"contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":4096}}]}]});
        value["providers"][0]["headers"]["CONTENT-TYPE"] = json!("application/json");
        let config = &mut value["providers"][0]["models"][0]["config"];
        match self.endpoint.kind {
            "anthropic" => {
                config["providerOptions"] =
                    json!({"anthropic":{"thinking":{"type":"enabled","budgetTokens":1024}}})
            }
            "gemini" => {
                config["providerOptions"] = json!({"gemini":{"thinkingConfig":{"thinkingBudget":1024,"includeThoughts":true}}})
            }
            "openai-responses" => {
                config["reasoning"] = json!({"effort":"low","summary":"detailed"})
            }
            _ => {}
        }
        Arc::new(AimuxModels::from_config(serde_json::from_value(value).unwrap()).unwrap())
    }
    async fn close(self) {
        self.cancel.cancel();
        self.join.await.unwrap();
    }
}
fn sse(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

#[tokio::test]
async fn malformed_native_block_lifecycle_is_a_classified_failure() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let server = MockServer::start().await;
    let text_start =
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}});
    let text_delta = json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"unexpected"}});
    let end = json!({"type":"content_block_stop","index":0});
    for (events, diagnostic) in [
        (
            vec![text_delta.clone()],
            "text-delta for text block `0` that was not started",
        ),
        (
            vec![
                text_start.clone(),
                text_delta.clone(),
                text_start.clone(),
                text_delta.clone(),
            ],
            "text block `0` started twice",
        ),
        (
            vec![text_start.clone(), text_delta.clone(), end, text_delta],
            "text-delta for text block `0` that was not started",
        ),
        (
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            ],
            "reasoning block `0` started twice",
        ),
        (
            vec![
                text_start,
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"wrong kind"}}),
            ],
            "reasoning-delta for reasoning block `0` that was not started",
        ),
        (
            vec![
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"missing start"}}),
            ],
            "reasoning-delta for reasoning block `0` that was not started",
        ),
    ] {
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(events)),
            )
            .mount(&server)
            .await;
        let failure =
            invoke_failure(&host, ModelProtocol::Anthropic, server.uri(), "malformed").await;
        assert!(
            matches!(
                &failure,
                HostError::Failed {
                    kind: HostErrorKind::Other,
                    ..
                }
            ),
            "{failure:?}"
        );
        let HostError::Failed {
            model: Some(model), ..
        } = &failure
        else {
            panic!("{failure:?}");
        };
        assert_eq!(model.kind, ModelFailureKind::MalformedResponse);
        assert!(
            failure.to_string().contains(diagnostic),
            "expected {diagnostic}: {failure}"
        );
    }
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn chat_reasoning_can_reopen_a_completed_segment_identity() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let server = MockServer::start().await;
    let body = frame(
        json!({"reasoning_content":"first", "content":""}),
        Value::Null,
    ) + &frame(json!({"content":"answer"}), Value::Null)
        + &frame(
            json!({"reasoning_content":" second", "content":""}),
            Value::Null,
        )
        + &frame(json!({}), json!("stop"))
        + "data: [DONE]\n\n";
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let mut stream = host
        .0
        .invoke(
            Arc::new(ModelRequest::new(
                "local",
                "test-model",
                vec![Message::text(Role::User, "question")],
            )),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::OpenaiCompatible,
                base_url: server.uri(),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut thoughts = vec![];
    let mut text = String::new();
    let mut stopped = false;
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            ModelChunk::ThinkingEnd { thinking } => thoughts.push(thinking),
            ModelChunk::Text(delta) => text.push_str(&delta),
            ModelChunk::MessageStop { .. } => stopped = true,
            _ => {}
        }
    }
    assert_eq!(thoughts, ["first", " second"]);
    assert_eq!(text, "answer");
    assert!(stopped);
    root.shutdown().await.unwrap();
}

async fn invoke_failure(
    host: &ModelHost,
    protocol: ModelProtocol,
    base_url: String,
    prompt: &str,
) -> HostError {
    let request = Arc::new(ModelRequest::new(
        "local",
        "test-model",
        vec![Message::text(Role::User, prompt)],
    ));
    let connection = ModelConnection::Http(HttpModelConnection {
        protocol,
        base_url,
        network: Default::default(),
        headers: Default::default(),
        bearer: None,
        api_key: None,
        preset: None,
    });
    match host
        .0
        .invoke(request, connection, CancellationToken::new())
        .await
    {
        Err(error) => error,
        Ok(mut stream) => loop {
            match stream.next().await {
                Some(Err(error)) => break error,
                Some(Ok(_)) => {}
                None => panic!("model stream ended without the expected failure"),
            }
        },
    }
}

/// Asserts the facts of one provider failure. `reason` is `None` for a
/// transport failure, whose innermost cause is platform text; that case checks
/// the chain itself.
fn assert_model_failure(
    error: &HostError,
    (status, code): (Option<u16>, Option<&str>),
    kind: ModelFailureKind,
    reason: Option<&str>,
    request_id: Option<&str>,
) {
    let HostError::Failed {
        kind:
            HostErrorKind::Http {
                status: actual_status,
                code: actual_code,
            },
        model: Some(model),
        ..
    } = error
    else {
        panic!("unexpected host error: {error:?}");
    };
    assert_eq!(*actual_status, status);
    assert_eq!(actual_code.as_deref(), code);
    assert_eq!(model.kind, kind);
    assert_eq!(model.status, status);
    assert_eq!(model.request_id.as_deref(), request_id);
    assert_eq!(model.attempts, 1);
    assert_eq!(model.waited, Duration::ZERO);
    assert_eq!(model.retry_after_stop, None);
    if let Some(reason) = reason {
        assert_eq!(model.reason.as_deref(), Some(reason));
        assert_eq!(model.cause.last().map(String::as_str), Some(reason));
    }
    let boundary = format!("{error:?} {error}");
    assert!(!boundary.contains("127.0.0.1"), "{boundary}");
    assert!(!boundary.contains("facts-"), "{boundary}");
    assert!(!boundary.contains("raw-body-must-not-cross"));
    assert!(!boundary.contains("private-header-must-not-cross"));
}

#[tokio::test]
async fn native_failures_carry_their_kind_and_a_safe_reason() {
    let chat = NativeServer::start("openai-chat").await;
    let responses = NativeServer::start("openai-responses").await;
    let anthropic = NativeServer::start("anthropic").await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();

    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiChat,
        chat.url.clone(),
        "facts-429",
    )
    .await;
    assert_model_failure(
        &error,
        (Some(429), Some("rate_limit_exceeded")),
        ModelFailureKind::RateLimited,
        Some("slow down"),
        Some("request-429"),
    );
    let HostError::Failed {
        model: Some(model), ..
    } = &error
    else {
        unreachable!()
    };
    assert_eq!(model.retry_after, Some(Duration::from_millis(26)));

    let blank = invoke_failure(
        &host,
        ModelProtocol::OpenaiChat,
        chat.url.clone(),
        "facts-blank",
    )
    .await;
    assert_model_failure(
        &blank,
        (Some(400), None),
        ModelFailureKind::InvalidRequest,
        Some("Mixed Message"),
        Some("Mixed-Request-ID"),
    );

    for (prompt, status, code, kind) in [
        (
            "facts-501",
            501,
            "server_error",
            ModelFailureKind::InvalidRequest,
        ),
        (
            "facts-quota",
            429,
            "insufficient_quota",
            ModelFailureKind::QuotaExhausted,
        ),
    ] {
        let error =
            invoke_failure(&host, ModelProtocol::OpenaiChat, chat.url.clone(), prompt).await;
        assert_model_failure(
            &error,
            (Some(status), Some(code)),
            kind,
            Some("request rejected"),
            Some("request-http"),
        );
        if kind == ModelFailureKind::QuotaExhausted {
            assert_eq!(
                error.to_string(),
                "The account has no quota left: request rejected. Check the plan or billing."
            );
        }
    }

    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiResponses,
        responses.url.clone(),
        "facts-responses-stream",
    )
    .await;
    assert_model_failure(
        &error,
        (Some(500), Some("server_error")),
        ModelFailureKind::ServerError,
        Some("responses unavailable"),
        Some("request-responses"),
    );

    let error = invoke_failure(
        &host,
        ModelProtocol::Anthropic,
        anthropic.url.clone(),
        "facts-anthropic-stream",
    )
    .await;
    assert_model_failure(
        &error,
        (Some(529), None),
        ModelFailureKind::ServerError,
        Some("anthropic overloaded"),
        Some("request-anthropic"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiChat,
        format!("http://{address}/v1"),
        "connection-refused",
    )
    .await;
    assert_model_failure(&error, (None, None), ModelFailureKind::Network, None, None);
    let HostError::Failed {
        model: Some(model), ..
    } = &error
    else {
        unreachable!()
    };
    let reason = model
        .reason
        .as_deref()
        .expect("a transport failure carries its innermost cause");
    assert!(model.cause.len() >= 2, "{:?}", model.cause);
    assert!(
        model.cause[0].contains("error sending request"),
        "{model:?}"
    );
    let line = error.to_string();
    assert!(
        line.starts_with("Could not reach the provider, or the connection dropped: "),
        "{line}"
    );
    assert!(line.contains(reason), "{line}");
    assert!(line.ends_with(" Check the network or proxy."), "{line}");

    root.shutdown().await.unwrap();
    chat.close().await;
    responses.close().await;
    anthropic.close().await;
}

#[tokio::test]
async fn native_http_statuses_and_codes_classify() {
    let chat = NativeServer::start("openai-chat").await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();

    for (prompt, status, kind) in [
        ("facts-status-401", 401, ModelFailureKind::AuthRejected),
        ("facts-status-402", 402, ModelFailureKind::QuotaExhausted),
        ("facts-status-404", 404, ModelFailureKind::NotFound),
        ("facts-status-408", 408, ModelFailureKind::ServerError),
        ("facts-status-409", 409, ModelFailureKind::InvalidRequest),
        ("facts-status-413", 413, ModelFailureKind::ContextTooLong),
        ("facts-status-429", 429, ModelFailureKind::RateLimited),
        ("facts-status-500", 500, ModelFailureKind::ServerError),
        ("facts-status-507", 507, ModelFailureKind::ServerError),
        ("facts-status-529", 529, ModelFailureKind::ServerError),
    ] {
        let error =
            invoke_failure(&host, ModelProtocol::OpenaiChat, chat.url.clone(), prompt).await;
        let reason = format!("fixture {status}");
        let request_id = format!("request-{status}");
        assert_model_failure(
            &error,
            (Some(status), None),
            kind,
            Some(&reason),
            Some(&request_id),
        );
    }

    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiChat,
        chat.url.clone(),
        "facts-policy",
    )
    .await;
    assert_model_failure(
        &error,
        (Some(400), Some("content_policy_violation")),
        ModelFailureKind::ContentFiltered,
        Some("fixture 400"),
        Some("request-400"),
    );

    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiChat,
        chat.url.clone(),
        "facts-max-tokens",
    )
    .await;
    assert_model_failure(
        &error,
        (Some(429), Some("max_tokens_exceeded")),
        ModelFailureKind::InvalidRequest,
        Some("request rejected"),
        Some("request-max-tokens"),
    );

    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiChat,
        chat.url.clone(),
        "facts-status-429-wait",
    )
    .await;
    assert_model_failure(
        &error,
        (Some(429), None),
        ModelFailureKind::RateLimited,
        Some("fixture 429"),
        Some("request-429"),
    );
    let HostError::Failed {
        model: Some(model), ..
    } = &error
    else {
        unreachable!()
    };
    assert_eq!(model.retry_after, Some(Duration::from_millis(2000)));

    chat.close().await;
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_malformed_stream_body_is_a_malformed_response() {
    let chat = NativeServer::start("openai-chat").await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();

    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiChat,
        chat.url.clone(),
        "facts-malformed-sse",
    )
    .await;
    let HostError::Failed {
        model: Some(model), ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::MalformedResponse);
    assert_eq!(model.status, None);
    assert!(model.cause.is_empty());
    let reason = model.reason.as_deref().expect("the parse diagnosis");
    // The error's message embeds the frame's text; the reason comes from the
    // parse error's cause, so the text is never stored.
    assert!(reason.contains("expected value"), "{reason}");
    let boundary = format!("{error:?} {error} {model:?}");
    assert!(!boundary.contains("body-text-must-not-cross"), "{boundary}");
    assert!(
        error
            .to_string()
            .starts_with("The provider sent a response Dim could not use"),
        "{error}"
    );

    chat.close().await;
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_reset_after_headers_is_a_network_failure() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = socket.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&chunk[..count]);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n5\r\ndata:\r\n",
            )
            .await
            .unwrap();
        socket.flush().await.unwrap();
        drop(socket);
    });

    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiChat,
        format!("http://{address}/v1"),
        "reset-after-headers",
    )
    .await;
    server.await.unwrap();
    let HostError::Failed {
        model: Some(model), ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::Network);
    assert_eq!(model.status, None);
    assert!(model.reason.is_some(), "{model:?}");
    assert!(!model.cause.is_empty(), "{model:?}");

    root.shutdown().await.unwrap();
}

/// A stream-event failure whose code is neither permanent nor transient is not
/// retried: the event's code decides, not the status the tag synthesizes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_event_failure_with_an_unknown_code_is_not_retried() {
    let responses = NativeServer::start("openai-responses").await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let error = invoke_failure(
        &host,
        ModelProtocol::OpenaiResponses,
        responses.url.clone(),
        "facts-unrecognized-stream",
    )
    .await;
    let HostError::Failed {
        model: Some(model), ..
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::Unknown);
    assert_eq!(
        responses.endpoint.requests.lock().unwrap().len(),
        1,
        "an unrecognized stream-event failure is not retried"
    );
    responses.close().await;
    root.shutdown().await.unwrap();
}

async fn respond_native(
    State(endpoint): State<NativeEndpoint>,
    uri: Uri,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    endpoint
        .requests
        .lock()
        .unwrap()
        .push((uri, headers, request.clone()));
    let request_text = request.to_string();
    if let Some(status) = request_text.split("facts-status-").nth(1).and_then(|rest| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u16>().ok()
    }) {
        let mut response = Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .header("x-request-id", format!("request-{status}"))
            .header("x-private-fixture", "private-header-must-not-cross");
        if request_text.contains("facts-status-429-wait") {
            response = response.header("retry-after", "2");
        }
        return response
            .body(Body::from(
                json!({
                    "error": {
                        "message": format!("fixture {status}"),
                        "fixture": "raw-body-must-not-cross"
                    }
                })
                .to_string(),
            ))
            .unwrap();
    }
    if request_text.contains("facts-policy") {
        return Response::builder()
            .status(400)
            .header("content-type", "application/json")
            .header("x-request-id", "request-400")
            .header("x-private-fixture", "private-header-must-not-cross")
            .body(Body::from(
                json!({
                    "error": {
                        "code": "content_policy_violation",
                        "message": "fixture 400",
                        "fixture": "raw-body-must-not-cross"
                    }
                })
                .to_string(),
            ))
            .unwrap();
    }
    if request_text.contains("facts-max-tokens") {
        return Response::builder()
            .status(429)
            .header("content-type", "application/json")
            .header("x-request-id", "request-max-tokens")
            .body(Body::from(
                json!({
                    "error": {
                        "code": "max_tokens_exceeded",
                        "message": "request rejected",
                        "fixture": "raw-body-must-not-cross"
                    }
                })
                .to_string(),
            ))
            .unwrap();
    }
    if request_text.contains("facts-malformed-sse") {
        return Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from("data: body-text-must-not-cross\n\n"))
            .unwrap();
    }
    let http_failure = if request_text.contains("facts-429") {
        Some((
            429,
            " Rate_Limit_Exceeded ",
            " Rate_Limit_Error ",
            "slow down",
            "request-429",
        ))
    } else if request_text.contains("facts-blank") {
        Some((400, " \t ", " ", " Mixed Message ", "Mixed-Request-ID"))
    } else if request_text.contains("facts-501") {
        Some((
            501,
            "server_error",
            "api_error",
            "request rejected",
            "request-http",
        ))
    } else if request_text.contains("facts-quota") {
        Some((
            429,
            " Insufficient_Quota ",
            "rate_limit_error",
            "request rejected",
            "request-http",
        ))
    } else {
        None
    };
    if let Some((status, code, error_type, message, request_id)) = http_failure {
        let mut response = Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .header("x-request-id", request_id)
            .header("x-private-fixture", "private-header-must-not-cross");
        if request_text.contains("facts-429") {
            response = response.header("retry-after-ms", "25.1");
        }
        return response
            .body(Body::from(
                json!({
                    "error": {
                        "code": code,
                        "type": error_type,
                        "message": message,
                        "fixture": "raw-body-must-not-cross"
                    }
                })
                .to_string(),
            ))
            .unwrap();
    }
    let continuation = match endpoint.kind {
        "openai-responses" => request["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output" || item["id"] == "refusal-1"),
        "gemini" => request["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| item["parts"].as_array().unwrap())
            .any(|part| part.get("functionResponse").is_some()),
        _ => request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    || message["content"]
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(|part| part["type"] == "tool_result"))
            }),
    };
    let missing_continuation = request.to_string().contains("missing-continuation");
    let missing_terminal = request.to_string().contains("native-eof");
    let missing_stop = request.to_string().contains("native-stop-eof");
    let data = match endpoint.kind {
        "openai-chat" | "openai-compatible" => {
            if continuation {
                frame(json!({"content":"finished"}), json!("stop")) + "data: [DONE]\n\n"
            } else {
                frame(json!({"content":"before"}), Value::Null)
                    + &frame(
                        json!({"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"echo","arguments":"{\"text\":\"result text\"}"}}]}),
                        json!("tool_calls"),
                    )
                    + "data: [DONE]\n\n"
            }
        }
        "anthropic" => {
            if request_text.contains("facts-anthropic-stream") {
                return Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .header("x-request-id", "request-anthropic")
                    .header("x-private-fixture", "private-header-must-not-cross")
                    .body(Body::from(sse(vec![json!({
                        "type": "error",
                        "error": {"type": "overloaded_error", "message": "anthropic overloaded", "fixture": "raw-body-must-not-cross"}
                    })])))
                    .unwrap();
            }
            let mut events = vec![
                json!({"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","model":"test-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":0}}}),
            ];
            if continuation {
                events.extend([json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}), json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"finished"}}), json!({"type":"content_block_stop","index":0})]);
            } else {
                events.extend([
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":if missing_continuation {""} else {"anthropic-signature"}}}), json!({"type":"content_block_stop","index":0}),
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"redacted-data"}}), json!({"type":"content_block_stop","index":1}),
                    json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}), json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"before"}}), json!({"type":"content_block_stop","index":2}),
                    json!({"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"call-1","name":"echo","input":{}}}), json!({"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{\"text\":\"result text\"}"}}), json!({"type":"content_block_stop","index":3}),
                ]);
            }
            events.extend([json!({"type":"message_delta","delta":{"stop_reason":if continuation {"end_turn"} else {"tool_use"},"stop_sequence":null},"usage":{"output_tokens":3}}), json!({"type":"message_stop"})]);
            if missing_terminal {
                events.retain(|event| {
                    event["type"] != "message_delta" && event["type"] != "message_stop"
                });
            } else if missing_stop {
                events.retain(|event| event["type"] != "message_stop");
            }
            sse(events)
        }
        "gemini" => {
            let parts = if continuation {
                json!([{"text":"finished"}])
            } else if request.to_string().contains("native-text-tool")
                || request.to_string().contains("native-thought-tool")
            {
                let thought = request.to_string().contains("native-thought-tool");
                json!([
                    {"text":"before","thought":thought,"thoughtSignature":"boundary-signature"},
                    {"functionCall":{"name":"echo","args":{"text":"result text"}},"thoughtSignature":"tool-signature"},
                    {"text":"after","thoughtSignature":"boundary-signature"}
                ])
            } else {
                json!([
                {"text":"thought","thought":true,"thoughtSignature":"thought-signature"},
                {"text":"before","thoughtSignature":"first-text-signature"},
                {"text":"after","thoughtSignature":"second-text-signature"},
                {"text":"","thoughtSignature":"empty-signature"},
                {"functionCall":{"name":"echo","args":{"text":"result text"}},"thoughtSignature":"tool-signature"}
                ])
            };
            let mut chunk = json!({"candidates":[{"content":{"role":"model","parts":parts},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3,"totalTokenCount":8}});
            if missing_terminal {
                chunk["candidates"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("finishReason");
            }
            sse(vec![chunk])
        }
        "openai-responses" => {
            if request_text.contains("facts-unrecognized-stream") {
                return Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(Body::from(sse(vec![
                        json!({"type":"response.created","response":{"id":"resp-error","created_at":1,"model":"test-model"}}),
                        json!({
                            "type": "response.failed",
                            "response": {"error": {"code": "unrecognized_failure", "type": "unexpected_failure", "message": "unrecognized"}}
                        }),
                    ])))
                    .unwrap();
            }
            if request_text.contains("facts-responses-stream") {
                return Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .header("x-request-id", "request-responses")
                    .header("x-private-fixture", "private-header-must-not-cross")
                    .body(Body::from(sse(vec![
                        json!({"type":"response.created","response":{"id":"resp-error","created_at":1,"model":"test-model"}}),
                        json!({
                            "type": "response.failed",
                            "response": {"error": {"code": "server_error", "type": "api_error", "message": "responses unavailable", "fixture": "raw-body-must-not-cross"}}
                        }),
                    ])))
                    .unwrap();
            }
            let mut events = vec![
                json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":"test-model"}}),
            ];
            let refusal = request.to_string().contains("native-refusal") && !continuation;
            if refusal {
                events.extend([
                    json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"refusal-1","role":"assistant","phase":"final_answer","content":[]}}),
                    json!({"type":"response.content_part.added","item_id":"refusal-1","output_index":0,"content_index":0,"part":{"type":"refusal","refusal":""}}),
                ]);
                if request.to_string().contains("native-refusal-empty") {
                    events.push(json!({"type":"response.refusal.delta","item_id":"refusal-1","output_index":0,"content_index":0,"delta":""}));
                } else if !request.to_string().contains("native-refusal-done") {
                    events.extend([
                        json!({"type":"response.refusal.delta","item_id":"refusal-1","output_index":0,"content_index":0,"delta":"Cannot "}),
                        json!({"type":"response.refusal.delta","item_id":"refusal-1","output_index":0,"content_index":0,"delta":"comply."}),
                    ]);
                }
                events.extend([
                    json!({"type":"response.refusal.done","item_id":"refusal-1","output_index":0,"content_index":0,"refusal":"Cannot comply."}),
                    json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"refusal-1","role":"assistant","phase":"final_answer","content":[{"type":"refusal","refusal":"Cannot comply."}]}}),
                ]);
            } else {
                if !continuation {
                    events.extend([
                    json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs-1","summary":[],"encrypted_content":null}}),
                    json!({"type":"response.reasoning_summary_part.added","item_id":"rs-1","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":""}}),
                    json!({"type":"response.reasoning_summary_text.delta","item_id":"rs-1","output_index":0,"summary_index":0,"delta":"first thought"}),
                    json!({"type":"response.reasoning_summary_part.done","item_id":"rs-1","output_index":0,"summary_index":0,"part":{"type":"summary_text","text":"first thought"}}),
                    json!({"type":"response.reasoning_summary_part.added","item_id":"rs-1","output_index":0,"summary_index":1,"part":{"type":"summary_text","text":""}}),
                    json!({"type":"response.reasoning_summary_text.delta","item_id":"rs-1","output_index":0,"summary_index":1,"delta":"second thought"}),
                    json!({"type":"response.reasoning_summary_part.done","item_id":"rs-1","output_index":0,"summary_index":1,"part":{"type":"summary_text","text":"second thought"}}),
                    json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs-1","encrypted_content":if missing_continuation {Value::Null} else {json!("encrypted-final")},"summary":[{"type":"summary_text","text":"first thought"},{"type":"summary_text","text":"second thought"}]}}),
                ]);
                }
                let output_index = if continuation { 0 } else { 1 };
                events.push(json!({"type":"response.output_item.added","output_index":output_index,"item":{"type":"message","id":"msg-1","role":"assistant","phase":"commentary","content":[]}}));
                for (index, text) in if continuation {
                    vec![(0, "finished")]
                } else {
                    vec![(0, "before"), (1, "after")]
                } {
                    events.extend([json!({"type":"response.content_part.added","item_id":"msg-1","output_index":output_index,"content_index":index,"part":{"type":"output_text","text":"","annotations":[]}}), json!({"type":"response.output_text.delta","item_id":"msg-1","output_index":output_index,"content_index":index,"delta":text}), json!({"type":"response.output_text.done","item_id":"msg-1","output_index":output_index,"content_index":index,"text":text})]);
                }
                events.push(json!({"type":"response.output_item.done","output_index":output_index,"item":{"type":"message","id":"msg-1","role":"assistant","phase":"commentary","content":[]}}));
                if !continuation {
                    events.extend([
                    json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc-1","call_id":"call-1","name":"echo","arguments":""}}),
                    json!({"type":"response.output_item.added","output_index":3,"item":{"type":"function_call","id":"fc-2","call_id":"call-2","name":"echo","arguments":""}}),
                    json!({"type":"response.function_call_arguments.delta","item_id":"fc-2","output_index":3,"delta":"{\"text\":\"second result\"}"}),
                    json!({"type":"response.function_call_arguments.done","item_id":"fc-2","output_index":3,"arguments":"{\"text\":\"second result\"}"}),
                    json!({"type":"response.output_item.done","output_index":3,"item":{"type":"function_call","id":"fc-2","call_id":"call-2","name":"echo","arguments":"{\"text\":\"second result\"}"}}),
                    json!({"type":"response.function_call_arguments.delta","item_id":"fc-1","output_index":2,"delta":"{\"text\":\"result text\"}"}),
                    json!({"type":"response.function_call_arguments.done","item_id":"fc-1","output_index":2,"arguments":"{\"text\":\"result text\"}"}),
                    json!({"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","id":"fc-1","call_id":"call-1","name":"echo","arguments":"{\"text\":\"result text\"}"}}),
                ]);
                }
            }
            events.push(json!({"type":"response.completed","response":{"id":"resp-1","created_at":1,"model":"test-model","status":"completed","usage":{"input_tokens":5,"output_tokens":3,"total_tokens":8}}}));
            if missing_terminal {
                events.retain(|event| event["type"] != "response.completed");
            }
            sse(events)
        }
        _ => unreachable!(),
    };
    let hold = endpoint.kind == "gemini" && !missing_terminal;
    // The body carries its length unless the case holds the connection open: a
    // client that stops reading at the stream's terminal frame then still
    // leaves the connection reusable for its next request.
    let body = if hold {
        Body::from_stream(async_stream::stream! {
            yield Ok::<_, Infallible>(data);
            std::future::pending::<()>().await;
        })
    } else {
        Body::from(data)
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn four_native_apis_preserve_signed_blocks_and_tool_continuation() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for kind in ["openai-chat","openai-responses","anthropic","gemini"] {
            let server = NativeServer::start(kind).await;
            let agent = model_agent(server.models(), None).await;
            let session = agent.create_session(server.session_config()).await.unwrap();
            session.input(InputMessageData {
                user: Some(UserInput { content: vec![
                    ContentBlock::Text { text: "tool".into() },
                    ContentBlock::Image { source: ImageSource { source_type: ImageSourceType::Base64, media_type: "image/png".into(), data: Some(IMAGE.into()), url: None } },
                ] }),
                mode: Some(InputMode::Queue),
                ..Default::default()
            }, "main").await.unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            assert!(matches!(events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })), "{kind}: {:?}", events.iter().filter_map(|e| if let EventKind::ModelOutputEnd(data)=&e.kind{Some(&data.outcome)}else{None}).collect::<Vec<_>>());
            assert!(events.iter().any(|e| matches!(&e.kind, EventKind::Text(data) if data.delta=="finished")));
            let requests = server.endpoint.requests.lock().unwrap().clone();
            assert_eq!(requests.len(),2,"{kind}");
            for (uri,headers,request) in &requests {
                assert!(uri.query().unwrap().contains("tenant=sandbox"),"{kind}: {uri}");
                let header = match kind {"anthropic"=>"x-api-key","gemini"=>"x-goog-api-key",_=>"authorization"};
                assert_eq!(headers.get(header).unwrap(), if header=="authorization" {"Bearer native-secret"} else {"native-secret"});
                assert_eq!(headers.get_all(header).iter().count(),1);
                assert_eq!(headers.get_all("content-type").iter().count(),1,"{kind}");
                assert!(!request.to_string().contains("native-secret"));
                if kind != "anthropic" {
                    assert!(!request.to_string().contains("cache_control"), "{kind}: {request}");
                }
            }
            let first = &requests[0].2;
            match kind {
                "anthropic" => assert_eq!(first["messages"].as_array().unwrap().iter().find(|message| message["role"]=="user").unwrap()["content"][1],json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":IMAGE},"cache_control":{"type":"ephemeral"}})),
                "gemini" => assert_eq!(*first["contents"].as_array().unwrap().iter().flat_map(|message| message["parts"].as_array().unwrap()).find(|part|part.get("inlineData").is_some()).unwrap(),json!({"inlineData":{"mimeType":"image/png","data":IMAGE}})),
                "openai-responses" => assert_eq!(first["input"].as_array().unwrap().iter().find(|message| message["role"]=="user").unwrap()["content"][1],json!({"type":"input_image","image_url":format!("data:image/png;base64,{IMAGE}")})),
                _ => assert_eq!(first["messages"].as_array().unwrap().iter().find(|message| message["role"]=="user").unwrap()["content"][1],json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{IMAGE}")}})),
            }
            let next = &requests[1].2;
            match kind {
                "anthropic" => {
                    let assistant = next["messages"].as_array().unwrap().iter().find(|m| m["role"]=="assistant").unwrap();
                    assert_eq!(assistant["content"][0], json!({"type":"thinking","thinking":"","signature":"anthropic-signature"}));
                    assert_eq!(assistant["content"][1], json!({"type":"redacted_thinking","data":"redacted-data"}));
                    assert_eq!(next["thinking"]["budget_tokens"],1024);
                }
                "gemini" => {
                    let model = next["contents"].as_array().unwrap().iter().find(|m|m["role"]=="model").unwrap();
                    assert_eq!(model["parts"].as_array().unwrap().len(),5);
                    assert_eq!(model["parts"][0]["thoughtSignature"],"thought-signature");
                    assert_eq!(model["parts"][1]["thoughtSignature"],"first-text-signature");
                    assert_eq!(model["parts"][2]["thoughtSignature"],"second-text-signature");
                    assert_eq!(model["parts"][3],json!({"text":"","thoughtSignature":"empty-signature"}));
                    assert_eq!(model["parts"][4]["thoughtSignature"],"tool-signature");
                    assert_eq!(next["generationConfig"]["thinkingConfig"],json!({"thinkingBudget":1024,"includeThoughts":true}));
                    assert!(requests[0].0.query().unwrap().contains("alt=sse"));
                }
                "openai-responses" => {
                    let input=next["input"].as_array().unwrap();
                    let reasoning=input.iter().find(|item|item["type"]=="reasoning").unwrap();
                    assert_eq!(reasoning["id"],"rs-1");
                    assert_eq!(reasoning["encrypted_content"],"encrypted-final");
                    assert_eq!(reasoning["summary"],json!([{"type":"summary_text","text":"first thought"},{"type":"summary_text","text":"second thought"}]));
                    assert_eq!(input.iter().filter(|item|item["id"]=="rs-1").count(),1);
                    let message=input.iter().find(|item|item["id"]=="msg-1").unwrap();
                    assert_eq!(message["phase"],"commentary");
                    assert_eq!(message["content"],json!([{"type":"output_text","text":"before"},{"type":"output_text","text":"after"}]));
                    assert_eq!(input.iter().find(|item|item["type"]=="function_call").unwrap()["id"],"fc-1");
                    assert_eq!(input.iter().filter(|item|item["type"]=="function_call").map(|item|item["id"].as_str().unwrap()).collect::<Vec<_>>(),vec!["fc-1","fc-2"]);
                    assert_eq!(next["store"],false);
                    assert!(next["include"].as_array().unwrap().contains(&json!("reasoning.encrypted_content")));
                }
                _ => assert_eq!(next["messages"].as_array().unwrap().last().unwrap()["content"],"result text"),
            }
            if kind!="openai-chat" {
                let content = events.iter().find(|e|matches!(e.kind,EventKind::ModelContent(_))).unwrap();
                assert!(dim_agent_types::v1::EventRef::new(content).is_none());
                for event in &events {
                    if matches!(event.kind,EventKind::ModelCall(_) | EventKind::ContextPrepare(_)) {
                        let wire=serde_json::to_value(dim_agent_types::v1::EventRef::new(event).unwrap()).unwrap();
                        assert!(!wire["data"]["request"]["messages"].to_string().contains("providerMetadata"));
                    }
                }
            }
            let host=agent.root().get::<ModelHost>().unwrap();
            let protocol=match kind {"openai-chat"=>ModelProtocol::OpenaiChat,"openai-responses"=>ModelProtocol::OpenaiResponses,"anthropic"=>ModelProtocol::Anthropic,_=>ModelProtocol::Gemini};
            let connection=||ModelConnection::Http(HttpModelConnection{protocol,base_url:server.url.clone(),network:Default::default(),headers:Default::default(),bearer:if kind=="anthropic" {Some("bearer-secret".into())} else {None},api_key:None,preset:None});
            let request=Arc::new(ModelRequest::new("local","test-model",vec![Message::text(Role::User,"tool")]));
            let mut unpolled=host.0.invoke(request.clone(),connection(),CancellationToken::new()).await.unwrap();
            let mut partial=host.0.invoke(request,connection(),CancellationToken::new()).await.unwrap();
            while !matches!(partial.next().await.unwrap().unwrap(),ModelChunk::Text(_)) {}
            if kind=="anthropic" {
                let requests=server.endpoint.requests.lock().unwrap();
                assert_eq!(requests.last().unwrap().1.get_all("authorization").iter().count(),1);
                assert_eq!(requests.last().unwrap().1["authorization"],"Bearer bearer-secret");
            }
            if matches!(kind, "anthropic" | "gemini") {
                let key = if kind == "anthropic" { "x-api-key" } else { "x-goog-api-key" };
                let mut custom = host.0.invoke(
                    Arc::new(ModelRequest::new("local", "test-model", vec![Message::text(Role::User, "tool")])),
                    ModelConnection::Http(HttpModelConnection {
                        protocol, base_url: server.url.clone(), headers: [("x-custom-auth".into(), "custom-secret".into())].into(),
                        network: Default::default(),
                        bearer: None, api_key: None, preset: None,
                    }), CancellationToken::new(),
                ).await.unwrap();
                while let Some(chunk) = custom.next().await { chunk.unwrap(); }
                let requests = server.endpoint.requests.lock().unwrap();
                let headers = &requests.last().unwrap().1;
                assert_eq!(headers["x-custom-auth"], "custom-secret");
                assert!(!headers.contains_key(key), "{kind} sent an absent API key");
                assert!(!headers.contains_key("authorization"));
            }
            agent.close().await.unwrap();
            for stream in [&mut unpolled,&mut partial] { assert!(matches!(stream.next().await,Some(Err(HostError::Failed{kind:HostErrorKind::OwnerClosed,..})))); }
            if kind == "gemini" {
                use aimux_core::{language_model::LanguageModel, language_model_message::{LanguageModelMessage, TextPart, UserPart}, options::CallOptions, stream_part::StreamPart, types::FinishReasonUnified};
                use aimux_provider_utils::Resolvable;
                use aimux_providers::vertex::{VertexProviderSettings, create_google_vertex};
                let provider = create_google_vertex(VertexProviderSettings {
                    api_key: Some(Resolvable::Value(String::new())),
                    base_url: Some(server.url.trim_end_matches("?tenant=sandbox").into()),
                    access_token: Some(Resolvable::Value("vertex-secret".into())),
                    ..VertexProviderSettings::default()
                }).unwrap();
                let model = provider.chat(server.model_id());
                let options = CallOptions {
                    prompt: vec![LanguageModelMessage::User {
                        content: vec![UserPart::Text(TextPart { text: "tool".into(), provider_options: None })],
                        provider_options: None,
                    }],
                    ..Default::default()
                };
                let result = model.do_stream(&options).await.unwrap();
                let parts = tokio::time::timeout(Duration::from_secs(1), result.stream.collect::<Vec<_>>()).await.expect("Vertex terminal must finish while HTTP stays open");
                assert!(parts.iter().any(|part| matches!(part, Ok(StreamPart::ToolCall(call)) if call.tool_name == "echo")));
                assert!(matches!(parts.last().unwrap(), Ok(StreamPart::Finish { finish_reason, usage, .. }) if finish_reason.unified == FinishReasonUnified::ToolCalls && usage.input_tokens.total == Some(5) && usage.output_tokens.total == Some(3)));
            }
            server.close().await;
        }
    }).await.unwrap();
}

struct SystemCacheBreaks {
    breaks: usize,
    timeline: rutis::host::Key<Timeline>,
}
impl rutis::host::DynPlugin for SystemCacheBreaks {
    fn name(&self) -> &str {
        "test.system-cache-breaks"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let breaks = self.breaks;
            timeline.before(ctx, self.name(), &["model/call"], move |mut event| {
                let EventKind::ModelCall(call) = &mut event.kind else {
                    unreachable!()
                };
                let request = Arc::make_mut(&mut call.request);
                request.system = Some(Arc::new(SystemPrompt::Parts(
                    (0..breaks)
                        .map(|index| SystemPromptPart::Text {
                            text: format!("system part {index}"),
                            cache_control: Some(CacheControl::Ephemeral),
                        })
                        .collect(),
                )));
                Ok(event)
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_requests_mark_the_system_and_the_conversation_tail_for_cache() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let server = NativeServer::start("anthropic").await;
        let agent = model_agent(server.models(), None).await;
        let session = agent.create_session(server.session_config()).await.unwrap();
        session
            .submit("tool".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session
            .submit("text".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let requests = server.endpoint.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 3);
        let breakpoint = json!({"type": "ephemeral"});
        for (_, _, request) in &requests {
            let system = request["system"].as_array().unwrap();
            assert_eq!(system.last().unwrap()["cache_control"], breakpoint);
            assert_eq!(
                request.to_string().matches("\"cache_control\"").count(),
                2,
                "{request}"
            );
            for tool in request["tools"].as_array().unwrap() {
                assert!(tool.get("cache_control").is_none(), "{tool}");
            }
            let last = request["messages"].as_array().unwrap().last().unwrap();
            assert_eq!(
                last["content"].as_array().unwrap().last().unwrap()["cache_control"],
                breakpoint,
                "{last}"
            );
        }
        let first = requests[0].2["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(first["role"], "user");
        assert_eq!(first["content"][0]["type"], "text");
        let second = requests[1].2["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(second["role"], "user");
        assert_eq!(second["content"][0]["type"], "tool_result");
        let third = requests[2].2["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(third["role"], "user");
        assert_eq!(third["content"][0]["type"], "text");
        for request in &requests {
            assert_eq!(request.2["system"], requests[0].2["system"]);
        }
        agent.close().await.unwrap();
        server.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_system_breakpoints_stay_within_the_four_breakpoint_limit() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for breaks in [2usize, 4, 5] {
            let emitted = breaks.min(4);
            let message_mark = usize::from(breaks < 4);
            let server = NativeServer::start("anthropic").await;
            let models = server.models();
            let mut builder = builder(models.clone(), Arc::new(MockTools));
            builder.require_feature(Feature::new("test.system-cache-breaks").branch(
                "test.system-cache-breaks",
                move |names| {
                    Ok(Box::new(SystemCacheBreaks {
                        breaks,
                        timeline: names.key()?,
                    }))
                },
            ));
            let agent = model_agent(models, Some(builder)).await;
            let session = agent.create_session(server.session_config()).await.unwrap();
            session
                .submit("tool".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            let last = &events
                .iter()
                .rfind(|event| matches!(event.kind, EventKind::TurnEnd(_)))
                .unwrap()
                .kind;
            if breaks < 5 {
                assert!(
                    matches!(
                        last,
                        EventKind::TurnEnd(TurnEndData {
                            outcome: TurnOutcome::Completed { .. },
                            ..
                        })
                    ),
                    "breaks {breaks}: {events:?}"
                );
            } else {
                assert!(
                    matches!(
                        last,
                        EventKind::TurnEnd(TurnEndData {
                            outcome: TurnOutcome::Failed { error, .. },
                            ..
                        }) if error.contains("cacheControl breakpoint limit")
                    ),
                    "breaks {breaks}: the dropped mark is reported as unsupported: {events:?}"
                );
            }
            let requests = server.endpoint.requests.lock().unwrap().clone();
            assert!(!requests.is_empty(), "breaks {breaks}");
            for (_, _, request) in &requests {
                let system = request["system"].as_array().unwrap();
                assert_eq!(system.len(), breaks, "breaks {breaks}: {request}");
                for (index, part) in system.iter().enumerate() {
                    if index < emitted {
                        assert_eq!(
                            part["cache_control"],
                            json!({"type": "ephemeral"}),
                            "breaks {breaks}: {request}"
                        );
                    } else {
                        assert!(
                            part.get("cache_control").is_none(),
                            "breaks {breaks}: {request}"
                        );
                    }
                }
                assert_eq!(
                    request.to_string().matches("\"cache_control\"").count(),
                    emitted + message_mark,
                    "breaks {breaks}: {request}"
                );
            }
            agent.close().await.unwrap();
            server.close().await;
        }
    })
    .await
    .unwrap();
}

struct ErrorEcho;
impl ToolDriver for ErrorEcho {
    fn definitions(&self) -> Vec<ToolDefinition> {
        MockTools.definitions()
    }
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async { Ok(ToolResult::error("tool failed")) })
    }
}
struct RewriteNative {
    complete: bool,
    timeline: rutis::host::Key<Timeline>,
}
impl rutis::host::DynPlugin for RewriteNative {
    fn name(&self) -> &str {
        "test.rewrite-native"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let key = if self.complete {
                "step/model-content"
            } else {
                "step/tool-use"
            };
            timeline.before(ctx, self.name(), &[key], |mut draft| {
                match &mut draft.kind {
                    EventKind::ToolUse(data) => {
                        data.call.input.insert("text".into(), json!("changed"));
                    }
                    EventKind::ModelContent(data) => data.content.clear(),
                    _ => unreachable!(),
                }
                Ok(draft)
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_unsigned_thinking_completes_and_is_not_replayed() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for kind in ["anthropic", "openai-responses"] {
            let server = NativeServer::start(kind).await;
            let agent = model_agent(server.models(), None).await;
            let session = agent.create_session(server.session_config()).await.unwrap();
            session
                .submit("missing-continuation".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            assert!(
                matches!(
                    events
                        .iter()
                        .rfind(|event| matches!(event.kind, EventKind::TurnEnd(_)))
                        .unwrap()
                        .kind,
                    EventKind::TurnEnd(TurnEndData {
                        outcome: TurnOutcome::Completed { .. },
                        ..
                    })
                ),
                "{kind}"
            );
            let content = events
                .iter()
                .find_map(|event| match &event.kind {
                    EventKind::ModelContent(data) => Some(data.content.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            // The reply keeps its thinking and text even without continuation data.
            assert!(
                content
                    .iter()
                    .any(|part| matches!(part, MessagePart::Thinking { .. })),
                "{kind}: {content:?}"
            );
            assert!(
                content
                    .iter()
                    .any(|part| matches!(part,MessagePart::Text{text,..} if text=="before")),
                "{kind}: {content:?}"
            );
            // Step 2 keeps the tool call and its result, and leaves the unsigned thinking out.
            let requests = server.endpoint.requests.lock().unwrap().clone();
            assert_eq!(requests.len(), 2, "{kind}");
            let next = &requests[1].2;
            match kind {
                "anthropic" => {
                    let assistant = next["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|message| message["role"] == "assistant")
                        .unwrap();
                    let parts = assistant["content"].as_array().unwrap();
                    assert!(
                        !parts.iter().any(|part| part["type"] == "thinking"),
                        "{kind}: {assistant}"
                    );
                    // Signed metadata is replayed exactly as before.
                    assert!(
                        parts.iter().any(|part| part["type"] == "redacted_thinking"),
                        "{kind}: {assistant}"
                    );
                    assert!(
                        parts.iter().any(|part| part["type"] == "tool_use"),
                        "{kind}: {assistant}"
                    );
                    assert!(
                        next["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .flat_map(|message| message["content"].as_array().unwrap())
                            .any(|part| part["type"] == "tool_result"),
                        "{kind}"
                    );
                    assert!(
                        !next.to_string().contains("\"signature\""),
                        "{kind}: {next}"
                    );
                }
                _ => {
                    let input = next["input"].as_array().unwrap();
                    assert!(
                        !input
                            .iter()
                            .any(|item| item["type"] == "reasoning" || item["id"] == "rs-1"),
                        "{kind}: {next}"
                    );
                    assert!(
                        input.iter().any(|item| item["type"] == "function_call"),
                        "{kind}"
                    );
                    assert!(
                        input
                            .iter()
                            .any(|item| item["type"] == "function_call_output"),
                        "{kind}"
                    );
                }
            }
            // A request built from stored content drops the part on its own.
            let host = agent.root().get::<ModelHost>().unwrap();
            let request = Arc::new(ModelRequest::new(
                "local",
                server.model_id(),
                vec![Message {
                    role: Role::Assistant,
                    content: MessageContent::Parts(vec![
                        MessagePart::Thinking {
                            thinking: "unreplayable".into(),
                            start_time: None,
                            end_time: None,
                            provider_metadata: None,
                        },
                        MessagePart::Text {
                            text: "kept".into(),
                            start_time: None,
                            end_time: None,
                            provider_metadata: None,
                        },
                    ]),
                }],
            ));
            let connection = ModelConnection::Http(HttpModelConnection {
                protocol: if kind == "anthropic" {
                    ModelProtocol::Anthropic
                } else {
                    ModelProtocol::OpenaiResponses
                },
                base_url: server.url.clone(),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            });
            let mut stream = host
                .0
                .invoke(request, connection, CancellationToken::new())
                .await
                .expect("unsigned thinking must not fail the request");
            while let Some(chunk) = stream.next().await {
                chunk.unwrap();
            }
            {
                let requests = server.endpoint.requests.lock().unwrap();
                assert_eq!(requests.len(), 3, "{kind}");
                let sent = requests[2].2.to_string();
                assert!(
                    !sent.contains("unreplayable") && !sent.contains("\"thinking\""),
                    "{kind}: {sent}"
                );
                assert!(sent.contains("kept"), "{kind}");
            }
            agent.close().await.unwrap();
            server.close().await;
        }
        // Bedrock: a stored unsigned block is metadata only, so the replay
        // carries neither it nor a `reasoningContent` for it.
        {
            use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
            let server = MockServer::start().await;
            let body = [
                bedrock_frame("messageStart", json!({"role":"assistant"})),
                bedrock_frame(
                    "contentBlockDelta",
                    json!({"contentBlockIndex":0,"delta":{"text":"kept"}}),
                ),
                bedrock_frame("contentBlockStop", json!({"contentBlockIndex":0})),
                bedrock_frame("messageStop", json!({"stopReason":"end_turn"})),
                bedrock_frame(
                    "metadata",
                    json!({"usage":{"inputTokens":1,"outputTokens":1,"totalTokens":2}}),
                ),
            ]
            .concat();
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "application/vnd.amazon.eventstream")
                        .set_body_bytes(body),
                )
                .mount(&server)
                .await;
            let root = rutis::Ctx::root().unwrap();
            root.plugin(AimuxPlugin).await.unwrap();
            let host = root.get::<ModelHost>().unwrap();
            let request = Arc::new(ModelRequest::new(
                "local",
                "arn:aws:bedrock:us-west-2:123:inference-profile/model-A",
                vec![Message {
                    role: Role::Assistant,
                    content: MessageContent::Parts(vec![
                        MessagePart::Thinking {
                            thinking: "unsigned".into(),
                            start_time: None,
                            end_time: None,
                            provider_metadata: Some(ContentMetadata::Bedrock { signature: None }),
                        },
                        MessagePart::Text {
                            text: "kept".into(),
                            start_time: None,
                            end_time: None,
                            provider_metadata: None,
                        },
                    ]),
                }],
            ));
            let connection = ModelConnection::Bedrock(BedrockModelConnection {
                base_url: server.uri(),
                network: Default::default(),
                headers: Default::default(),
                auth: BedrockAuth::Bearer("bedrock-secret".into()),
            });
            let mut stream = host
                .0
                .invoke(request, connection, CancellationToken::new())
                .await
                .unwrap();
            while let Some(chunk) = stream.next().await {
                chunk.unwrap();
            }
            let sent = server.received_requests().await.unwrap();
            assert_eq!(sent.len(), 1);
            let sent: Value = serde_json::from_slice(&sent[0].body).unwrap();
            assert!(
                !sent.to_string().contains("reasoningContent")
                    && !sent.to_string().contains("unsigned"),
                "{sent}"
            );
            assert!(sent.to_string().contains("kept"), "{sent}");
            root.shutdown().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_content_keeps_unsigned_thinking_and_respects_committed_tools() {
    tokio::time::timeout(Duration::from_secs(20),async{
        let server=NativeServer::start("anthropic").await;
        let models=server.models();
        for complete in [false,true]{
            let mut custom=builder(models.clone(),Arc::new(MockTools));
            custom.require_feature(Feature::new("test.rewrite-native").branch("test.rewrite-native", move|names|Ok(Box::new(RewriteNative{complete,timeline:names.key()?}))));
            let agent=model_agent(models.clone(),Some(custom)).await;
            let session=agent.create_session(server.session_config()).await.unwrap();
            session.submit("tool".into(),InputMode::Queue,"main").await.unwrap();session.idle("main").await.unwrap();
            let events=chain_events(&session, "main", ..).await.unwrap();
            assert!(events.iter().any(|e|matches!(&e.kind,EventKind::ModelOutputEnd(data) if matches!(data.outcome,ModelOutputOutcome::Failed{..}))));
            // Calls the failed output started are closed, never dispatched (agent-029).
            assert!(!events.iter().any(|e|matches!(&e.kind,EventKind::ToolCall(_)) || matches!(&e.kind,EventKind::ToolResult(data) if data.call_event_id.is_some())));
            agent.close().await.unwrap();
        }
        let custom=builder(models.clone(),Arc::new(ErrorEcho));
        let agent=model_agent(models,Some(custom)).await;let session=agent.create_session(server.session_config()).await.unwrap();
        session.submit("tool".into(),InputMode::Queue,"main").await.unwrap();session.idle("main").await.unwrap();
        let requests=server.endpoint.requests.lock().unwrap().clone();
        let last=&requests.last().unwrap().2;
        let result=last["messages"].as_array().unwrap().iter().flat_map(|m|m["content"].as_array().unwrap()).find(|part|part["type"]=="tool_result").unwrap();
        assert_eq!(result["is_error"],true);assert_eq!(result["content"],"tool failed");
        agent.close().await.unwrap();server.close().await;
        for kind in ["anthropic", "openai-responses"] {
            let server = NativeServer::start(kind).await;
            let agent = model_agent(server.models(), None).await;
            let session = agent.create_session(server.session_config()).await.unwrap();
            session.submit("missing-continuation".into(),InputMode::Queue,"main").await.unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            // The reply completes and commits its complete content.
            assert!(matches!(events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind,EventKind::TurnEnd(TurnEndData{outcome:TurnOutcome::Completed{..},..})),"{kind}");
            assert!(events.iter().any(|event|matches!(&event.kind,EventKind::ModelContent(_))),"{kind}");
            if kind == "anthropic" {
                let host = agent.root().get::<ModelHost>().unwrap();
                // Thinking off: stored signed thinking is left out of the
                // request instead of failing it.
                let request=Arc::new(ModelRequest::new("local",server.model_id(),vec![
                    Message::text(Role::User,"question"),
                    Message {role:Role::Assistant,content:MessageContent::Parts(vec![
                        MessagePart::Thinking {thinking:"signed".into(),start_time:None,end_time:None,provider_metadata:Some(ContentMetadata::Anthropic {signature:"original-signature".into()})},
                        MessagePart::Text {text:"kept".into(),start_time:None,end_time:None,provider_metadata:None},
                    ])}
                ]));
                let connection=ModelConnection::Http(HttpModelConnection {protocol:ModelProtocol::Anthropic,base_url:server.url.clone(),network:Default::default(),headers:Default::default(),bearer:None,api_key:None,preset:None});
                let before=server.endpoint.requests.lock().unwrap().len();
                let mut stream=host.0.invoke(request,connection,CancellationToken::new()).await.expect("stored thinking with thinking off must not fail the request");
                while let Some(chunk)=stream.next().await{chunk.unwrap();}
                let requests=server.endpoint.requests.lock().unwrap();
                assert_eq!(requests.len(),before+1);
                let sent=requests.last().unwrap().2.to_string();
                assert!(!sent.contains("original-signature"),"the signature reached the request: {sent}");
                assert!(sent.contains("kept"),"{sent}");
            }
            agent.close().await.unwrap();server.close().await;
        }
        for (kind, prompt) in [
            ("openai-responses", "native-eof"),
            ("anthropic", "native-eof"),
            ("anthropic", "native-stop-eof"),
            ("gemini", "native-eof"),
        ] {
            let server = NativeServer::start(kind).await;
            let agent = model_agent(server.models(), None).await;
            let session = agent.create_session(server.session_config()).await.unwrap();
            session.submit(prompt.into(), InputMode::Queue, "main").await.unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            assert!(events.iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.starts_with("before"))), "{kind}: {prompt}");
            // Strict mode holds a client tool call until the finish and
            // drops it when the stream fails, so it does not appear in the
            // history. At main it appeared but was never executed.
            assert!(!events.iter().any(|event| matches!(event.kind, EventKind::ToolUse(_))), "{kind}: {prompt}");
            assert!(events.iter().any(|event| matches!(&event.kind, EventKind::ModelOutputEnd(data) if matches!(data.outcome, ModelOutputOutcome::Failed { .. }))), "{kind}: {prompt}");
            assert!(!events.iter().any(|event| matches!(&event.kind, EventKind::ModelContent(_) | EventKind::ToolCall(_) | EventKind::ModelUsage(_)) || matches!(&event.kind, EventKind::ToolResult(data) if data.call_event_id.is_some())), "{kind}: {prompt}");
            // The failed reply announced a call and ran none of it, so the
            // turn takes its one continuation. The fixture cuts every request
            // that carries the prompt, so that request fails the same way and
            // the turn ends on its outcome; what the case pins is the extra
            // request and that neither one dispatched the announced call.
            assert_eq!(server.endpoint.requests.lock().unwrap().len(), 2, "{kind}: {prompt}: the cut request and its continuation");
            let terminals: Vec<_> = events.iter().filter_map(|event| match &event.kind { EventKind::ModelOutputEnd(data) => Some(&data.outcome), _ => None }).collect();
            assert_eq!(terminals.len(), 2, "{kind}: {prompt}");
            assert!(matches!(events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind,
                EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Failed { ref error }, .. }) if matches!(&terminals[1], ModelOutputOutcome::Failed { error: last, .. } if last == error)),
                "{kind}: {prompt}");
            agent.close().await.unwrap();
            server.close().await;
        }
        for prompt in ["native-refusal", "native-refusal-done", "native-refusal-empty"] {
            let server = NativeServer::start("openai-responses").await;
            let agent = model_agent(server.models(), None).await;
            let session = agent.create_session(server.session_config()).await.unwrap();
            session.submit(prompt.into(), InputMode::Queue, "main").await.unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            let visible: String = events.iter().filter_map(|event| match &event.kind { EventKind::Text(data) => Some(data.delta.as_str()), _ => None }).collect();
            assert_eq!(visible, "Cannot comply.");
            assert!(matches!(events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })));
            session.submit("follow-up".into(), InputMode::Queue, "main").await.unwrap();
            session.idle("main").await.unwrap();
            let requests = server.endpoint.requests.lock().unwrap().clone();
            assert_eq!(requests.len(), 2);
            let replay = requests[1].2["input"].as_array().unwrap().iter().find(|item| item["id"] == "refusal-1").unwrap();
            assert_eq!(replay["content"], json!([{"type":"output_text","text":"Cannot comply."}]));
            assert_eq!(replay["phase"], "final_answer");
            let events = chain_events(&session, "main", ..).await.unwrap();
            assert!(events.iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta == "finished")));
            agent.close().await.unwrap();
            server.close().await;
        }
        for prompt in ["native-text-tool", "native-thought-tool"] {
            let server = NativeServer::start("gemini").await;
            let agent = model_agent(server.models(), None).await;
            let session = agent.create_session(server.session_config()).await.unwrap();
            session.submit(prompt.into(), InputMode::Queue, "main").await.unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            assert!(matches!(events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })), "{prompt}");
            let requests = server.endpoint.requests.lock().unwrap().clone();
            assert_eq!(requests.len(), 2);
            let model = requests[1].2["contents"].as_array().unwrap().iter().find(|item| item["role"] == "model").unwrap();
            let parts = model["parts"].as_array().unwrap();
            assert_eq!(parts.len(), 3, "{prompt}: {parts:?}");
            assert_eq!(parts[0]["text"], "before");
            assert_eq!(parts[0]["thoughtSignature"], "boundary-signature");
            assert_eq!(parts[0].get("thought").and_then(Value::as_bool).unwrap_or(false), prompt == "native-thought-tool");
            assert_eq!(parts[1]["functionCall"]["name"], "echo");
            assert_eq!(parts[1]["thoughtSignature"], "tool-signature");
            assert_eq!(parts[2]["text"], "after");
            assert_eq!(parts[2]["thoughtSignature"], "boundary-signature");
            agent.close().await.unwrap();
            server.close().await;
        }
        let server = NativeServer::start("gemini").await;
        let agent = model_agent(server.models(),None).await;
        agent.create_session(server.session_config()).await.unwrap();
        let host = agent.root().get::<ModelHost>().unwrap();
        for (config, expected_budget) in [
            (json!({"reasoning":{"enabled":true,"budgetTokens":0}}),None),
            (json!({"reasoning":{"enabled":true},"providerOptions":{"gemini":{"thinkingConfig":{"thinkingBudget":0}}}}),Some(0)),
            (json!({"reasoning":{"budgetTokens":-99},"providerOptions":{"gemini":{"thinkingConfig":{"thinkingBudget":1024}}}}),Some(1024)),
        ] {
            let mut request=ModelRequest::new("local",server.model_id(),vec![Message::text(Role::User,"settings")]);
            request.config=Some(serde_json::from_value(config).unwrap());
            let connection=ModelConnection::Http(HttpModelConnection {protocol:ModelProtocol::Gemini,base_url:server.url.clone(),network:Default::default(),headers:Default::default(),bearer:None,api_key:None,preset:None});
            let before=server.endpoint.requests.lock().unwrap().len();
            match host.0.invoke(Arc::new(request),connection,CancellationToken::new()).await {
                Ok(mut stream)=> {
                    assert!(expected_budget.is_some());
                    assert!(matches!(stream.next().await,Some(Ok(ModelChunk::MessageStart {..}))));
                    let requests=server.endpoint.requests.lock().unwrap();
                    assert_eq!(requests.len(),before+1);
                    assert_eq!(requests.last().unwrap().2["generationConfig"]["thinkingConfig"]["thinkingBudget"],expected_budget.unwrap());
                }
                Err(error)=> {
                    assert!(expected_budget.is_none());
                    assert!(error.to_string().contains("conflicting reasoning"));
                    assert_eq!(server.endpoint.requests.lock().unwrap().len(),before);
                }
            }
        }
        agent.close().await.unwrap();server.close().await;
    }).await.unwrap();
}

/// #1038: a Session that ran on an Anthropic model moves to a Chat model and
/// back. The stored signed thinking only reaches the protocol that produced
/// it, and moving back continues it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_model_switch_keeps_stored_thinking_for_its_protocol() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let claude = NativeServer::start("anthropic").await;
        let chat = NativeServer::start("openai-compatible").await;
        let models = Arc::new(
            AimuxModels::from_config(
                serde_json::from_value(json!({
                    "defaultModel": {"providerId": "local", "modelId": "test-model"},
                    "providers": [
                        {
                            "id": "local", "kind": "anthropic", "baseUrl": claude.url,
                            "headers": {"x-api-key": "native-secret", "CONTENT-TYPE": "application/json"},
                            "models": [{"id": "test-model", "contextWindow": 128000, "maxOutputTokens": 4096,
                                        "config": {"maxTokens": 4096, "providerOptions": {"anthropic": {"thinking": {"type": "enabled", "budgetTokens": 1024}}}}}]
                        },
                        {
                            "id": "chat", "kind": "openai-compatible", "baseUrl": chat.url,
                            "headers": {"Authorization": "Bearer native-secret", "CONTENT-TYPE": "application/json"},
                            "models": [{"id": "test-model", "contextWindow": 128000, "maxOutputTokens": 4096,
                                        "config": {"maxTokens": 4096}}]
                        }
                    ]
                }))
                .unwrap(),
            )
            .unwrap(),
        );
        let agent = model_agent(models, None).await;
        let session = agent
            .create_session(claude.session_config())
            .await
            .unwrap();
        session
            .submit("question".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(
            matches!(
                events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind,
                EventKind::TurnEnd(TurnEndData {
                    outcome: TurnOutcome::Completed { .. },
                    ..
                })
            ),
            "{:?}",
            events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind
        );
        assert_eq!(claude.endpoint.requests.lock().unwrap().len(), 2);
        let selection = |provider_id: &str| {
            let provider_id = provider_id.to_owned();
            let host = agent.root().get::<ModelSelectionHost>().unwrap();
            async move {
                host.0
                    .resolve(
                        Some(ModelId {
                            provider_id,
                            model_id: "test-model".into(),
                        }),
                        None,
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap()
            }
        };

        // The Session moves to a Chat model; the second turn completes and its
        // request carries the earlier answer without the Anthropic signature.
        session
            .configure(
                ConfigureUpdate {
                    model: Some(selection("chat").await),
                    ..Default::default()
                },
                "main",
            )
            .await
            .unwrap();
        session
            .submit("after the switch".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(
            matches!(
                events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind,
                EventKind::TurnEnd(TurnEndData {
                    outcome: TurnOutcome::Completed { .. },
                    ..
                })
            ),
            "{:?}",
            events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind
        );
        let requests = chat.endpoint.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        let sent = requests[0].2.to_string();
        assert!(!sent.contains("signature"), "the signature reached the Chat model: {sent}");
        assert!(sent.contains("before"), "the earlier answer is kept: {sent}");
        assert!(sent.contains("finished"), "the earlier answer is kept: {sent}");
        assert!(sent.contains("after the switch"), "{sent}");

        // Moving back with thinking on replays the signed thinking.
        session
            .configure(
                ConfigureUpdate {
                    model: Some(selection("local").await),
                    ..Default::default()
                },
                "main",
            )
            .await
            .unwrap();
        session
            .submit("back".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(
            matches!(
                events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind,
                EventKind::TurnEnd(TurnEndData {
                    outcome: TurnOutcome::Completed { .. },
                    ..
                })
            ),
            "{:?}",
            events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind
        );
        let requests = claude.endpoint.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 3);
        let replay = &requests[2].2;
        let thinking = replay["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "assistant")
            .flat_map(|message| message["content"].as_array().unwrap())
            .find(|part| part["type"] == "thinking")
            .expect("the signed thinking is replayed");
        assert_eq!(thinking["signature"], "anthropic-signature");
        assert_eq!(replay["thinking"]["budget_tokens"], 1024);
        agent.close().await.unwrap();
        claude.close().await;
        chat.close().await;
    })
    .await
    .unwrap();
}

/// #1038: a Session whose Chat model stored thinking without continuation
/// data moves to Bedrock. The turn completes and the thinking is left out of
/// the Bedrock request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_bedrock_leaves_stored_chat_thinking_out_of_the_turn() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    tokio::time::timeout(Duration::from_secs(20), async {
        let server = MockServer::start().await;
        let body = [
            bedrock_frame("messageStart", json!({"role": "assistant"})),
            bedrock_frame(
                "contentBlockDelta",
                json!({"contentBlockIndex": 0, "delta": {"text": "answer"}}),
            ),
            bedrock_frame("contentBlockStop", json!({"contentBlockIndex": 0})),
            bedrock_frame("messageStop", json!({"stopReason": "end_turn"})),
            bedrock_frame(
                "metadata",
                json!({"usage": {"inputTokens": 5, "outputTokens": 3}}),
            ),
        ]
        .concat();
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/vnd.amazon.eventstream")
                    .set_body_bytes(body),
            )
            .mount(&server)
            .await;
        let root = rutis::Ctx::root().unwrap();
        root.plugin(AimuxPlugin).await.unwrap();
        let host = root.get::<ModelHost>().unwrap();
        // The history a Chat model leaves behind: thinking without data.
        let request = ModelRequest::new(
            "bedrock",
            "anthropic.model",
            vec![
                Message::text(Role::User, "question"),
                Message {
                    role: Role::Assistant,
                    content: MessageContent::Parts(vec![
                        MessagePart::Thinking {
                            thinking: "why".into(),
                            start_time: None,
                            end_time: None,
                            provider_metadata: None,
                        },
                        MessagePart::Text {
                            text: "earlier answer".into(),
                            start_time: None,
                            end_time: None,
                            provider_metadata: None,
                        },
                    ]),
                },
                Message::text(Role::User, "again"),
            ],
        );
        let mut output = host
            .0
            .invoke(
                Arc::new(request),
                ModelConnection::Bedrock(BedrockModelConnection {
                    base_url: server.uri(),
                    network: Default::default(),
                    headers: Default::default(),
                    auth: BedrockAuth::Bearer("bearer".into()),
                }),
                CancellationToken::new(),
            )
            .await
            .expect("missing Bedrock thinking data must not fail the request");
        let mut text = String::new();
        while let Some(chunk) = output.next().await {
            if let ModelChunk::Text(delta) = chunk.unwrap() {
                text.push_str(&delta);
            }
        }
        assert_eq!(text, "answer");
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let sent = String::from_utf8(requests[0].body.clone()).unwrap();
        assert!(!sent.contains("reasoningContent"), "{sent}");
        assert!(sent.contains("earlier answer"), "{sent}");
        root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

/// A tool whose result carries a text block and a PNG, as an MCP image result
/// does (mcp/server.rs maps an image content block into `ToolContent::Blocks`).
struct ImageEcho;
impl ToolDriver for ImageEcho {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            risk: Some("session".into()),
            ..dim_agent_capabilities::tool_registry::definition(
                "echo",
                "Return a text block and a PNG image.",
                json!({"type":"object","required":["text"],"additionalProperties":false,"properties":{"text":{"type":"string"}}}),
            )
        }]
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        assert_eq!(call.name, "echo");
        Box::pin(async move {
            Ok(ToolResult {
                content: ToolContent::Blocks(vec![
                    ContentBlock::Text {
                        text: "result text".into(),
                    },
                    ContentBlock::Image {
                        source: ImageSource {
                            source_type: ImageSourceType::Base64,
                            media_type: "image/png".into(),
                            data: Some(IMAGE.into()),
                            url: None,
                        },
                    },
                ]),
                structured_content: None,
                is_error: None,
                blob_refs: None,
                background_task: None,
                context_messages: None,
                effects: None,
            })
        })
    }
}

const TOOL_IMAGE_MARKER: &str = "[Image: sent in the following user message as tool output.]";
const TOOL_IMAGE_LABEL: &str = "Tool output from tool call";

fn tool_result_events(events: &[Arc<AgentEvent>]) -> Vec<&ToolContent> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(&data.results[0].result.content),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_images_take_the_form_its_protocol_defines() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for kind in ["openai-chat", "openai-responses", "anthropic", "gemini"] {
            let server = NativeServer::start(kind).await;
            let models = server.models();
            let agent = model_agent(models.clone(), Some(builder(models, Arc::new(ImageEcho)))).await;
            let session = agent.create_session(server.session_config()).await.unwrap();
            session.submit("call the tool".into(), InputMode::Queue, "main").await.unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            assert!(
                matches!(events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })),
                "{kind}: {:?}",
                events.iter().filter_map(|event| match &event.kind { EventKind::ModelOutputEnd(data) => Some(&data.outcome), _ => None }).collect::<Vec<_>>()
            );
            for content in tool_result_events(&events) {
                assert!(
                    matches!(content, ToolContent::Blocks(blocks) if blocks.iter().any(|block| matches!(block, ContentBlock::Image { .. }))),
                    "{kind}: the stored tool result keeps the image"
                );
            }
            let requests = server.endpoint.requests.lock().unwrap().clone();
            assert_eq!(requests.len(), 2, "{kind}");
            let next = &requests[1].2;
            match kind {
                "anthropic" => {
                    let result = next["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .flat_map(|message| message["content"].as_array().into_iter().flatten())
                        .find(|part| part["type"] == "tool_result")
                        .unwrap();
                    assert_eq!(
                        result["content"],
                        json!([
                            {"type":"text","text":"result text"},
                            {"type":"image","source":{"type":"base64","media_type":"image/png","data":IMAGE}},
                        ]),
                        "{kind}"
                    );
                    assert!(!next.to_string().contains(TOOL_IMAGE_MARKER), "{kind}: no marker when the protocol carries the image inside");
                }
                "gemini" => {
                    let contents = next["contents"].as_array().unwrap();
                    let response = contents
                        .iter()
                        .flat_map(|content| content["parts"].as_array().into_iter().flatten())
                        .find(|part| part.get("functionResponse").is_some())
                        .unwrap();
                    assert!(
                        response["functionResponse"]["response"]["content"].as_str().unwrap().contains(TOOL_IMAGE_MARKER),
                        "{kind}: {response}"
                    );
                    let user = contents.last().unwrap();
                    assert_eq!(user["role"], "user", "{kind}");
                    let parts = user["parts"].as_array().unwrap();
                    assert!(parts[0]["text"].as_str().unwrap().starts_with(TOOL_IMAGE_LABEL), "{kind}: {}", parts[0]);
                    assert_eq!(parts[1], json!({"inlineData":{"mimeType":"image/png","data":IMAGE}}), "{kind}");
                }
                "openai-responses" => {
                    let input = next["input"].as_array().unwrap();
                    let output = input.iter().find(|item| item["type"] == "function_call_output").unwrap();
                    assert!(output["output"].as_str().unwrap().contains(TOOL_IMAGE_MARKER), "{kind}: {output}");
                    let user = input.iter().rev().find(|item| item["role"] == "user").unwrap();
                    let parts = user["content"].as_array().unwrap();
                    assert!(parts[0]["text"].as_str().unwrap().starts_with(TOOL_IMAGE_LABEL), "{kind}: {}", parts[0]);
                    assert_eq!(parts[1], json!({"type":"input_image","image_url":format!("data:image/png;base64,{IMAGE}")}), "{kind}");
                }
                _ => {
                    let messages = next["messages"].as_array().unwrap();
                    let tool = messages.iter().find(|message| message["role"] == "tool").unwrap();
                    assert!(tool["content"].as_str().unwrap().contains(TOOL_IMAGE_MARKER), "{kind}: {tool}");
                    let user = messages.last().unwrap();
                    assert_eq!(user["role"], "user", "{kind}");
                    let parts = user["content"].as_array().unwrap();
                    assert_eq!(parts[0]["text"], format!("{TOOL_IMAGE_LABEL} call-1: the image after this text is that call's result."), "{kind}");
                    assert_eq!(parts[1], json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{IMAGE}")}}), "{kind}");
                }
            }
            session.submit("another".into(), InputMode::Queue, "main").await.unwrap();
            session.idle("main").await.unwrap();
            assert!(
                matches!(chain_events(&session, "main", ..).await.unwrap().iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })),
                "{kind}: a later turn also completes"
            );
            agent.close().await.unwrap();
            server.close().await;
        }
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_without_image_input_gets_the_placeholder_in_a_tool_result() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let server = NativeServer::start("openai-chat").await;
        let models = server.models();
        let agent = model_agent(models.clone(), Some(builder(models, Arc::new(ImageEcho)))).await;
        let mut config = server.session_config();
        config.model.capability_snapshot = Some(ModelCapabilitySnapshot {
            target: ModelId {
                provider_id: "local".into(),
                model_id: server.model_id().into(),
            },
            capabilities: ModelCapabilities { vision: false, ..ModelCapabilities::with_limits(128_000, 4_096) },
            reasoning: None,
            service_tier: None,
        });
        let session = agent.create_session(config).await.unwrap();
        session.submit("call the tool".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(matches!(events.iter().rfind(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })));
        for content in tool_result_events(&events) {
            assert!(
                matches!(content, ToolContent::Blocks(blocks) if blocks.iter().any(|block| matches!(block, ContentBlock::Image { .. }))),
                "the stored tool result keeps the image the request replaced"
            );
        }
        let requests = server.endpoint.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        let next = &requests[1].2;
        let tool = next["messages"].as_array().unwrap().iter().find(|message| message["role"] == "tool").unwrap();
        assert!(
            tool["content"].as_str().unwrap().contains("[Image omitted: the current model does not accept images.]"),
            "{tool}"
        );
        assert!(!next.to_string().contains("image_url") && !next.to_string().contains("data:image"), "no image in the wire body: {next}");
        agent.close().await.unwrap();
        server.close().await;
    }).await.unwrap();
}

/// The request the two inside-form protocols get in the test below: one
/// assistant tool call and one error tool result that carries an image.
fn error_tool_result_request() -> ModelRequest {
    let mut input = serde_json::Map::new();
    input.insert("text".into(), json!("result text"));
    let mut request = ModelRequest::new(
        "local",
        "test-model",
        vec![
            Message::text(Role::User, "question"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![MessagePart::ToolUse {
                    provider_metadata: None,
                    id: "call-1".into(),
                    name: "echo".into(),
                    input,
                    start_time: None,
                }]),
            },
            Message {
                role: Role::ToolResult,
                content: MessageContent::Parts(vec![MessagePart::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: ToolContent::Blocks(vec![
                        ContentBlock::Text {
                            text: "result text".into(),
                        },
                        ContentBlock::Image {
                            source: ImageSource {
                                source_type: ImageSourceType::Base64,
                                media_type: "image/png".into(),
                                data: Some(IMAGE.into()),
                                url: None,
                            },
                        },
                    ]),
                    structured_content: None,
                    is_error: Some(true),
                }]),
            },
        ],
    );
    // Bedrock keeps tool content only while a tool is active.
    request.tools = Some(MockTools.definitions().into());
    request
}

/// An error tool result that carries an image keeps the image, for each
/// protocol whose tool message carries the image inside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_error_tool_result_image_keeps_the_flag_and_the_image() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    json!({"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","model":"test-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":0}}}),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}),
                    json!({"type":"message_stop"}),
                ])),
        )
        .mount(&server)
        .await;
    let mut stream = host
        .0
        .invoke(
            Arc::new(error_tool_result_request()),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::Anthropic,
                base_url: server.uri(),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    let sent = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    let result = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|part| part["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["is_error"], json!(true), "{body}");
    assert_eq!(
        result["content"],
        json!([
            {"type":"text","text":"result text"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":IMAGE}},
        ]),
        "{body}"
    );
    assert!(
        !body.to_string().contains(TOOL_IMAGE_MARKER),
        "an inside-form result needs no follow-up marker: {body}"
    );

    let server = MockServer::start().await;
    let body = [
        bedrock_frame("messageStart", json!({"role":"assistant"})),
        bedrock_frame(
            "contentBlockDelta",
            json!({"contentBlockIndex":0,"delta":{"text":"ok"}}),
        ),
        bedrock_frame("contentBlockStop", json!({"contentBlockIndex":0})),
        bedrock_frame("messageStop", json!({"stopReason":"end_turn"})),
        bedrock_frame(
            "metadata",
            json!({"usage":{"inputTokens":1,"outputTokens":1,"totalTokens":2}}),
        ),
    ]
    .concat();
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(body),
        )
        .mount(&server)
        .await;
    let mut stream = host
        .0
        .invoke(
            Arc::new(error_tool_result_request()),
            ModelConnection::Bedrock(BedrockModelConnection {
                base_url: server.uri(),
                network: Default::default(),
                headers: Default::default(),
                auth: BedrockAuth::Bearer("native-secret".into()),
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    let sent = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    let result = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|part| part.get("toolResult").is_some())
        .unwrap();
    assert_eq!(
        result["toolResult"]["content"],
        json!([
            {"text":"result text"},
            {"image":{"format":"png","source":{"bytes":IMAGE}}},
        ]),
        "{body}"
    );
    assert_eq!(result["toolResult"]["status"], "error", "{body}");

    root.shutdown().await.unwrap();
}

/// A native stream that ends without its own terminal event is an invalid
/// response: classified as such, and not retried.
#[tokio::test]
async fn a_native_stream_without_its_terminal_is_classified_and_not_retried() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    json!({"type":"message_start","message":{"id":"m","type":"message","role":"assistant","model":"test-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":0}}}),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}),
                ])),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let failure =
        invoke_failure(&host, ModelProtocol::Anthropic, server.uri(), "no-terminal").await;
    assert!(
        matches!(
            &failure,
            HostError::Failed {
                kind: HostErrorKind::Other,
                ..
            }
        ),
        "{failure:?}"
    );
    let HostError::Failed {
        model: Some(model), ..
    } = &failure
    else {
        panic!("{failure:?}");
    };
    assert_eq!(model.kind, ModelFailureKind::MalformedResponse);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "an invalid response is not retried"
    );
    root.shutdown().await.unwrap();
}

/// A thinking part stored by an older session carries `summaryIndex`; the
/// event still loads, and the reasoning item continues.
#[tokio::test]
async fn a_stored_summary_index_loads_and_continues() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    // What the event store holds, as a session written before the upgrade:
    // the part in its wire shape, not one the current serializer produced.
    let stored = json!({
        "type": "thinking",
        "thinking": "earlier summary",
        "providerMetadata": {
            "protocol": "openai-responses",
            "itemId": "rs-old",
            "encryptedContent": "opaque",
            "summaryIndex": 1
        }
    });
    let loaded: MessagePart = serde_json::from_value(stored).unwrap();
    let MessagePart::Thinking {
        thinking,
        start_time,
        end_time,
        provider_metadata,
    } = &loaded
    else {
        panic!("a stored thinking part loads as thinking: {loaded:?}");
    };
    assert_eq!(thinking, "earlier summary");
    assert_eq!(start_time, &None);
    assert_eq!(end_time, &None);
    assert_eq!(
        provider_metadata,
        &Some(ContentMetadata::OpenaiResponses {
            item_id: "rs-old".into(),
            encrypted_content: Some("opaque".into()),
            summary_index: Some(1),
            phase: None,
        })
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":"alias"}}),
                    json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":1}}}),
                ])),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let request = ModelRequest::new(
        "local",
        "alias",
        vec![
            Message::text(Role::User, "start"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![loaded]),
            },
        ],
    );
    let mut stream = host
        .0
        .invoke(
            Arc::new(request),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::OpenaiResponses,
                base_url: server.uri(),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    let sent = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    let reasoning = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    assert_eq!(reasoning["id"], "rs-old");
    assert_eq!(reasoning["encrypted_content"], "opaque");
    assert_eq!(
        reasoning["summary"],
        json!([{"type":"summary_text","text":"earlier summary"}])
    );
    root.shutdown().await.unwrap();
}

/// The `xai` protocol is served by the OpenAI-compatible Chat model: the
/// request goes to `/chat/completions` and carries the xAI options dim sends.
#[tokio::test]
async fn an_xai_chat_request_uses_the_compatible_chat_path() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    frame(json!({"content":"visible"}), json!("stop")) + "data: [DONE]\n\n",
                ),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let mut request = ModelRequest::new(
        "xai",
        "fixture-model",
        vec![Message::text(Role::User, "hello")],
    );
    request.config = Some(
        serde_json::from_value(json!({"providerOptions":{"xai":{"reasoningEffort":"high"}}}))
            .unwrap(),
    );
    let mut stream = host
        .0
        .invoke(
            Arc::new(request),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::Xai,
                base_url: format!("{}/v1", server.uri()),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
    let sent = server.received_requests().await.unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url.path(), "/v1/chat/completions");
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(body["model"], "fixture-model");
    assert_eq!(body["reasoning_effort"], "high");
    root.shutdown().await.unwrap();
}

/// An alias with an explicit reasoning setting becomes a reasoning model, so a
/// temperature is unsupported; dim fails with that classification.
#[tokio::test]
async fn an_alias_with_explicit_reasoning_and_temperature_is_unsupported() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":"alias"}}),
                    json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":1}}}),
                ])),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let mut request = ModelRequest::new("local", "alias", vec![Message::text(Role::User, "hello")]);
    request.config = Some(
        serde_json::from_value(json!({"temperature":0.2,"reasoning":{"effort":"high"}})).unwrap(),
    );
    let mut stream = host
        .0
        .invoke(
            Arc::new(request),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::OpenaiResponses,
                base_url: server.uri(),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let failure = loop {
        match stream.next().await {
            Some(Err(failure)) => break failure,
            Some(Ok(_)) => {}
            None => panic!("the request did not fail"),
        }
    };
    assert!(
        matches!(
            &failure,
            HostError::Failed {
                kind: HostErrorKind::Unsupported,
                ..
            }
        ),
        "{failure:?}"
    );
    assert!(failure.to_string().contains("temperature"), "{failure}");
    root.shutdown().await.unwrap();
}

/// A Chat refusal delta becomes visible text (vendored patch for
/// arcships/aimux#461).
#[tokio::test]
async fn chat_refusal_delta_is_visible_text() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let server = MockServer::start().await;
    let body = frame(json!({"refusal":"I cannot help with that."}), Value::Null)
        + &frame(json!({}), json!("stop"))
        + "data: [DONE]\n\n";
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let mut stream = host
        .0
        .invoke(
            Arc::new(ModelRequest::new(
                "local",
                "test-model",
                vec![Message::text(Role::User, "question")],
            )),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::OpenaiCompatible,
                base_url: server.uri(),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut text = String::new();
    let mut stop = None;
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            ModelChunk::Text(delta) => text.push_str(&delta),
            ModelChunk::MessageStop { reason, .. } => stop = Some(reason),
            _ => {}
        }
    }
    assert_eq!(text, "I cannot help with that.");
    assert_eq!(stop, Some(ModelStopReason::EndTurn));
    root.shutdown().await.unwrap();
}

/// A Responses refusal reaches the reply as visible text (vendored patch for
/// arcships/aimux#466).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_refusal_deltas_are_visible_text() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let server = MockServer::start().await;
    let body = sse(vec![
        json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":"test-model"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"refusal-1","role":"assistant","phase":"final_answer","content":[]}}),
        json!({"type":"response.content_part.added","item_id":"refusal-1","output_index":0,"content_index":0,"part":{"type":"refusal","refusal":""}}),
        json!({"type":"response.refusal.delta","item_id":"refusal-1","output_index":0,"content_index":0,"delta":"Cannot "}),
        json!({"type":"response.refusal.delta","item_id":"refusal-1","output_index":0,"content_index":0,"delta":"comply."}),
        json!({"type":"response.refusal.done","item_id":"refusal-1","output_index":0,"content_index":0,"refusal":"Cannot comply."}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"refusal-1","role":"assistant","phase":"final_answer","content":[{"type":"refusal","refusal":"Cannot comply."}]}}),
        json!({"type":"response.completed","response":{"id":"resp-1","created_at":1,"model":"test-model","usage":{"input_tokens":1,"output_tokens":2}}}),
    ]);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let mut stream = host
        .0
        .invoke(
            Arc::new(ModelRequest::new(
                "local",
                "test-model",
                vec![Message::text(Role::User, "question")],
            )),
            ModelConnection::Http(HttpModelConnection {
                protocol: ModelProtocol::OpenaiResponses,
                base_url: server.uri(),
                network: Default::default(),
                headers: Default::default(),
                bearer: None,
                api_key: None,
                preset: None,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        if let ModelChunk::Text(delta) = chunk.unwrap() {
            text.push_str(&delta);
        }
    }
    assert_eq!(text, "Cannot comply.");
    root.shutdown().await.unwrap();
}

/// Signed Gemini text parts keep their own blocks, and text closes before a
/// function call (vendored patch for arcships/aimux#467).
#[tokio::test]
async fn gemini_signed_text_parts_keep_their_blocks() {
    use aimux_core::language_model::LanguageModel;
    use aimux_core::language_model_message::convert_to_language_model_prompt;
    use aimux_core::message::ModelMessage;
    use aimux_core::options::CallOptions;
    use aimux_core::stream_part::StreamPart;
    use aimux_providers::google::{GoogleProviderSettings, create_google};
    use futures::StreamExt;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let server = MockServer::start().await;
    let chunk = json!({"candidates":[{"content":{"role":"model","parts":[
        {"text":"before","thoughtSignature":"sig-before"},
        {"text":"after","thoughtSignature":"sig-after"},
        {"functionCall":{"id":"call-1","name":"echo","args":{"text":"x"}},"thoughtSignature":"sig-tool"}
    ]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}});
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![chunk])),
        )
        .mount(&server)
        .await;
    let model = create_google(GoogleProviderSettings {
        api_key: Some("test-key".into()),
        base_url: Some(format!("{}/v1beta", server.uri())),
        ..Default::default()
    })
    .unwrap()
    .generative_ai("gemini-2.5-pro");
    let options = CallOptions::new(
        convert_to_language_model_prompt(&[ModelMessage::user("hi")], None, None).unwrap(),
    );
    let mut result = model.do_stream(&options).await.unwrap();
    let mut trace = Vec::new();
    while let Some(part) = result.stream.next().await {
        match part.unwrap() {
            StreamPart::TextStart { id, .. } => trace.push(format!("start:{id}")),
            StreamPart::TextEnd { .. } => trace.push("end".into()),
            StreamPart::ToolCall(call) => trace.push(format!("tool:{}", call.tool_call_id)),
            StreamPart::Finish { .. } => trace.push("finish".into()),
            _ => {}
        }
    }
    let starts = trace
        .iter()
        .filter(|line| line.starts_with("start:"))
        .count();
    assert_eq!(starts, 2, "{trace:?}");
    let end = trace
        .iter()
        .position(|line| line == "end")
        .expect("text closes");
    let tool = trace
        .iter()
        .position(|line| line.starts_with("tool:"))
        .expect("tool call");
    assert!(
        end < tool,
        "text must close before the function call: {trace:?}"
    );
}

/// Gemini signs the text it follows: the tag's two recorded streams carry the
/// text in unsigned chunks and the `thoughtSignature` in a last, empty chunk,
/// so that signature belongs to the text before it and is replayed with it.
/// The streams are `fixtures/google-text.chunks.txt` and
/// `fixtures/google-reasoning.chunks.txt` under `vendor/aimux`; the tag's
/// tests at `aimux-providers/src/google/tests/language_model.rs:4429` and
/// `:4546` stream them and snapshot the result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gemini_signature_after_unsigned_text_signs_that_text_and_replays() {
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Endpoint {
        requests: Arc<Mutex<Vec<(Uri, Value)>>>,
    }
    async fn respond(
        State(endpoint): State<Endpoint>,
        uri: Uri,
        Json(request): Json<Value>,
    ) -> Response {
        let turn = {
            let mut requests = endpoint.requests.lock().unwrap();
            requests.push((uri, request));
            requests.len()
        };
        let chunks = if turn == 1 {
            vec![
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"first token "}]},"index":0}]}),
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"second token"}]},"index":0}]}),
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"","thoughtSignature":"signature-after-text"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":2,"totalTokenCount":3}}),
            ]
        } else {
            vec![
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"finished"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}}),
            ]
        };
        Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from(sse(chunks)))
            .unwrap()
    }

    let endpoint = Endpoint::default();
    let router = Router::new()
        .route("/v1beta/{*path}", post(respond))
        .with_state(endpoint.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1beta", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let join = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await
            .unwrap();
    });
    let models = Arc::new(
        AimuxModels::from_config(
            serde_json::from_value(json!({
                "defaultModel":{"providerId":"local","modelId":"test-model"},
                "providers":[{"id":"local","kind":"gemini","baseUrl":url,"headers":{"x-goog-api-key":"test-key"},"models":[{"id":"test-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":4096}}]}]
            }))
            .unwrap(),
        )
        .unwrap(),
    );
    let agent = model_agent(models, None).await;
    let session = agent.create_session(session_config()).await.unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(
        matches!(
            events
                .iter()
                .rfind(|event| matches!(event.kind, EventKind::TurnEnd(_)))
                .unwrap()
                .kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Completed { .. },
                ..
            })
        ),
        "{events:?}"
    );
    let content = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelContent(data) => Some(data.content.clone()),
            _ => None,
        })
        .unwrap_or_default();
    // The empty signed chunk signs the open block; it does not start one.
    assert_eq!(content.len(), 1, "{content:?}");
    assert!(
        matches!(
            &content[0],
            MessagePart::Text {
                text,
                provider_metadata: Some(ContentMetadata::Gemini { thought_signature }),
                ..
            } if text == "first token second token" && thought_signature == "signature-after-text"
        ),
        "{content:?}"
    );
    session
        .submit("second".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let requests = endpoint.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "{}", requests.len());
    let parts = requests[1].1["contents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "model")
        .unwrap()["parts"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        parts,
        vec![json!({"text":"first token second token","thoughtSignature":"signature-after-text"})],
        "{}",
        requests[1].1
    );
    drop(session);
    cancel.cancel();
    join.await.unwrap();
}

/// Responses text blocks split by content part, and a function call keeps its
/// item id (vendored patch for arcships/aimux#468).
#[tokio::test]
async fn responses_output_text_parts_and_function_call_ids_are_kept() {
    use aimux_core::language_model::LanguageModel;
    use aimux_core::language_model_message::convert_to_language_model_prompt;
    use aimux_core::message::ModelMessage;
    use aimux_core::options::CallOptions;
    use aimux_core::stream_part::StreamPart;
    use aimux_providers::openai::{OpenAIProviderSettings, create_openai};
    use futures::StreamExt;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let server = MockServer::start().await;
    let body = sse(vec![
        json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":"test-model"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","phase":"commentary","content":[]}}),
        json!({"type":"response.content_part.added","item_id":"msg-1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
        json!({"type":"response.output_text.delta","item_id":"msg-1","output_index":0,"content_index":0,"delta":"before"}),
        json!({"type":"response.content_part.added","item_id":"msg-1","output_index":0,"content_index":1,"part":{"type":"output_text","text":"","annotations":[]}}),
        json!({"type":"response.output_text.delta","item_id":"msg-1","output_index":0,"content_index":1,"delta":"after"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","phase":"commentary","content":[]}}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc-1","call_id":"call-1","name":"echo","arguments":""}}),
        json!({"type":"response.function_call_arguments.done","item_id":"fc-1","output_index":1,"arguments":"{\"text\":\"x\"}"}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc-1","call_id":"call-1","name":"echo","arguments":"{\"text\":\"x\"}"}}),
        json!({"type":"response.completed","response":{"id":"resp-1","created_at":1,"model":"test-model","usage":{"input_tokens":1,"output_tokens":1}}}),
    ]);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let model = create_openai(OpenAIProviderSettings {
        api_key: Some("test-key".into()),
        base_url: Some(format!("{}/v1", server.uri())),
        ..Default::default()
    })
    .unwrap()
    .responses("test-model");
    let options = CallOptions::new(
        convert_to_language_model_prompt(&[ModelMessage::user("hi")], None, None).unwrap(),
    );
    let mut result = model.do_stream(&options).await.unwrap();
    let mut text_starts = 0;
    let mut tool_item_id = None;
    while let Some(part) = result.stream.next().await {
        match part.unwrap() {
            StreamPart::TextStart { .. } => text_starts += 1,
            StreamPart::ToolInputStart {
                provider_metadata, ..
            } => {
                tool_item_id = provider_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("openai"))
                    .and_then(|namespace| namespace.get("itemId"))
                    .cloned();
            }
            _ => {}
        }
    }
    assert_eq!(text_starts, 2, "each output_text part keeps its own block");
    assert_eq!(
        tool_item_id,
        Some(json!("fc-1")),
        "the function call keeps its item id"
    );
}

/// A Gemini terminal chunk finishes the stream while the HTTP body stays open
/// (vendored patch for arcships/aimux#469).
#[tokio::test]
async fn gemini_terminal_chunk_finishes_while_the_body_stays_open() {
    use aimux_core::language_model::LanguageModel;
    use aimux_core::language_model_message::convert_to_language_model_prompt;
    use aimux_core::message::ModelMessage;
    use aimux_core::options::CallOptions;
    use aimux_core::stream_part::StreamPart;
    use aimux_providers::google::{GoogleProviderSettings, create_google};
    use futures::StreamExt;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = socket.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&chunk[..count]);
        }
        let body = "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"done\"}]},\"finishReason\":\"STOP\",\"index\":0}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1,\"totalTokenCount\":2}}\n\n";
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n{body}"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let model = create_google(GoogleProviderSettings {
        api_key: Some("test-key".into()),
        base_url: Some(format!("http://{address}/v1beta")),
        ..Default::default()
    })
    .unwrap()
    .generative_ai("gemini-2.5-pro");
    let options = CallOptions::new(
        convert_to_language_model_prompt(&[ModelMessage::user("hi")], None, None).unwrap(),
    );
    let mut result = model.do_stream(&options).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(2), async {
        let mut finished = false;
        while let Some(part) = result.stream.next().await {
            if matches!(part.unwrap(), StreamPart::Finish { .. }) {
                finished = true;
                break;
            }
        }
        finished
    })
    .await;
    server.abort();
    assert!(
        matches!(outcome, Ok(true)),
        "the terminal chunk must finish the stream while the body stays open"
    );
}

/// A TCP proxy in front of a local stub that counts the client connections it
/// accepts, so a test can tell a reused keep-alive connection from a new
/// client.
struct CountingProxy {
    url: String,
    accepted: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    join: tokio::task::JoinHandle<()>,
}

impl CountingProxy {
    async fn start(target: std::net::SocketAddr) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = accepted.clone();
        let join = tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    return;
                };
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await else {
                    return;
                };
                tokio::spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        Self {
            url,
            accepted,
            join,
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// One request, drained to its end: what matters is the transport it used.
async fn drain_request(host: &ModelHost, base_url: String, timeout_ms: Option<u64>) {
    let connection = ModelConnection::Http(HttpModelConnection {
        protocol: ModelProtocol::OpenaiChat,
        base_url,
        network: ModelNetwork {
            timeout_ms,
            ..ModelNetwork::default()
        },
        headers: Default::default(),
        bearer: None,
        api_key: Some("test-key".into()),
        preset: None,
    });
    let request = ModelRequest::new("local", "test-model", vec![Message::text(Role::User, "hi")]);
    let mut stream = host
        .0
        .invoke(Arc::new(request), connection, CancellationToken::new())
        .await
        .unwrap();
    while let Some(chunk) = stream.next().await {
        chunk.unwrap();
    }
}

/// A connection whose base URL carries a query, or whose network names a
/// timeout, keeps the driver's transport and its connection pool: two requests
/// with each use the one accepted TCP connection the driver holds.
#[tokio::test]
async fn a_connection_reuses_one_tcp_connection_for_its_query_and_timeout() {
    let server = NativeServer::start("openai-chat").await;
    let target: std::net::SocketAddr = server
        .url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let proxy = CountingProxy::start(target).await;
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();

    for _ in 0..2 {
        drain_request(&host, format!("{}/v1?tenant=sandbox", proxy.url), None).await;
    }
    assert_eq!(proxy.accepted(), 1, "the query connection reuses its pool");
    for _ in 0..2 {
        drain_request(&host, format!("{}/v1", proxy.url), Some(5_000)).await;
    }
    assert_eq!(
        proxy.accepted(),
        1,
        "the timeout connection reuses the driver's pool"
    );
    proxy.join.abort();
    server.close().await;
}

//! The stream and conversion contracts the aimux crates keep, pinned from the
//! adapter crate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimux_core::error::AiMuxError;
use aimux_core::language_model::LanguageModel;
use aimux_core::language_model_message::{FilePart, LanguageModelMessage, UserPart};
use aimux_core::options::CallOptions;
use aimux_core::shared::{FileBytes, FileData};
use aimux_core::stream_part::StreamPart;
use aimux_core::types::FinishReasonUnified;
use aimux_provider_utils::Resolvable;
use aimux_providers::bedrock::event_stream_decoder::encode_message;
use aimux_providers::openai_compatible::{
    OpenAICompatibleProviderSettings, create_openai_compatible,
};
use aimux_providers::{
    AmazonBedrockProviderSettings, PresetSettings, create_amazon_bedrock, create_provider,
};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Outcome = Result<StreamPart, AiMuxError>;

fn sse(events: &[Value]) -> Vec<u8> {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!("data: {event}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body.into_bytes()
}

fn frame(name: &str, payload: Value) -> Vec<u8> {
    encode_message("event", name, &payload.to_string())
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
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
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
    }
    bytes[header_end..header_end + length].to_vec()
}

/// One request served with `body`; the request's JSON body is kept.
async fn serve(body: Vec<u8>, content_type: &'static str) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let requests = seen.clone();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        requests
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&request).unwrap_or(Value::Null));
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(&body).await.unwrap();
    });
    (format!("http://{address}"), seen)
}

async fn collect<M: LanguageModel + ?Sized>(model: &M, options: CallOptions) -> Vec<Outcome> {
    let mut stream = model.do_stream(&options).await.unwrap().stream;
    let mut parts = Vec::new();
    while let Some(part) = stream.next().await {
        parts.push(part);
    }
    parts
}

fn chat_model(base: &str, name: &str) -> impl LanguageModel {
    create_openai_compatible(OpenAICompatibleProviderSettings {
        name: name.into(),
        base_url: format!("{base}/v1"),
        api_key: Some("test-key".into()),
        include_usage: Some(true),
        ..Default::default()
    })
    .unwrap()
    .chat("gpt-4o")
}

fn finish_usage(parts: &[Outcome]) -> Option<aimux_core::types::Usage> {
    parts.iter().rev().find_map(|part| match part {
        Ok(StreamPart::Finish { usage, .. }) => Some(usage.clone()),
        _ => None,
    })
}

/// Null `error` fields do not interrupt the stream; a non-null one reports an
/// API failure and the finish reason becomes Error.
#[tokio::test]
async fn chat_null_errors_are_ignored_and_stream_errors_fail() {
    for name in ["openai", "xai"] {
        let body = sse(&[
            json!({"error":null,"choices":[{"index":0,"delta":{"content":"first"},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{"content":" second"},"finish_reason":"stop"}],"error":null}),
            json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":7},"error":null}),
        ]);
        let (base, _) = serve(body, "text/event-stream").await;
        let parts = collect(&chat_model(&base, name), CallOptions::new(vec![])).await;
        let text: String = parts
            .iter()
            .filter_map(|part| match part {
                Ok(StreamPart::TextDelta { delta, .. }) => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "first second", "{name}");
        let last = parts.last().unwrap();
        assert!(
            matches!(last, Ok(StreamPart::Finish { finish_reason, usage, .. })
                if finish_reason.unified == FinishReasonUnified::Stop
                    && usage.input_tokens.total == Some(3)
                    && usage.output_tokens.total == Some(7)),
            "{name}: {parts:?}"
        );

        let body = sse(&[
            json!({"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}),
            json!({"error":{"message":"quota exhausted","code":"quota_exhausted"}}),
        ]);
        let (base, _) = serve(body, "text/event-stream").await;
        let parts = collect(&chat_model(&base, name), CallOptions::new(vec![])).await;
        assert!(
            parts.iter().any(|part| matches!(
                part,
                Ok(StreamPart::Error { error: AiMuxError::ApiCall(detail) })
                    if detail.message == "quota exhausted"
                        && detail.provider_code.as_deref() == Some("quota_exhausted")
            )),
            "{name}: {parts:?}"
        );
        assert!(
            matches!(parts.last(), Some(Ok(StreamPart::Finish { finish_reason, .. }))
                if finish_reason.unified == FinishReasonUnified::Error),
            "{name}: {parts:?}"
        );
    }
}

/// `response.completed` finishes the stream even when the HTTP body stays open.
#[tokio::test]
async fn xai_responses_finishes_before_the_http_body_closes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (release, wait) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        let body = sse(&[
            json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":0,"output_tokens":0}}}),
        ]);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        socket
            .write_all(format!("{:x}\r\n", body.len()).as_bytes())
            .await
            .unwrap();
        socket.write_all(&body).await.unwrap();
        socket.write_all(b"\r\n").await.unwrap();
        let _ = wait.await;
    });
    let model = aimux_providers::xai::create_xai(aimux_providers::xai::XAIProviderSettings {
        api_key: Some("test-key".into()),
        base_url: Some(url),
        headers: None,
        fetch: None,
    })
    .unwrap()
    .responses("grok-test");
    let parts = tokio::time::timeout(
        Duration::from_secs(3),
        collect(&model, CallOptions::new(vec![])),
    )
    .await
    .expect("the terminal event must finish before HTTP EOF");
    let _ = release.send(());
    server.await.unwrap();
    assert!(
        matches!(parts.last(), Some(Ok(StreamPart::Finish { finish_reason, .. }))
            if finish_reason.unified == FinishReasonUnified::Stop),
        "{parts:?}"
    );
}

/// A missing usage object keeps every count unknown; so does a present
/// object whose count the provider did not report. A reported zero stays
/// zero, on the compatible model and on the groq package (the tag's
/// successor to the earlier integration's `x_groq` profile).
#[tokio::test]
async fn chat_usage_keeps_unreported_counts_unknown() {
    let no_usage = sse(&[json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})]);
    let (base, _) = serve(no_usage, "text/event-stream").await;
    let parts = collect(&chat_model(&base, "openai"), CallOptions::new(vec![])).await;
    let usage = finish_usage(&parts).unwrap();
    assert_eq!(usage.input_tokens.total, None);
    assert_eq!(usage.output_tokens.total, None);
    assert_eq!(usage.input_tokens.no_cache, None);
    assert_eq!(usage.output_tokens.text, None);
    assert_eq!(usage.input_tokens.cache_read, None);
    assert_eq!(usage.output_tokens.reasoning, None);

    // usage, input total, no-cache total, output total, text total
    let cases = [
        (json!({}), None, None, None, None),
        (json!({"prompt_tokens": 2}), Some(2), Some(2), None, None),
        (
            json!({"completion_tokens": 0}),
            None,
            None,
            Some(0),
            Some(0),
        ),
    ];
    for (raw, input, no_cache, output, text) in cases.clone() {
        let body = sse(&[
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":raw}),
        ]);
        let (base, _) = serve(body, "text/event-stream").await;
        let parts = collect(&chat_model(&base, "openai"), CallOptions::new(vec![])).await;
        let usage = finish_usage(&parts).unwrap();
        assert_eq!(usage.input_tokens.total, input, "openai {raw}");
        assert_eq!(usage.input_tokens.no_cache, no_cache, "openai {raw}");
        assert_eq!(usage.output_tokens.total, output, "openai {raw}");
        assert_eq!(usage.output_tokens.text, text, "openai {raw}");
        assert_eq!(usage.input_tokens.cache_read, None, "openai {raw}");
        assert_eq!(usage.output_tokens.reasoning, None, "openai {raw}");
    }

    for (raw, input, no_cache, output, text) in cases {
        let body = sse(&[
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            json!({"choices":[],"x_groq":{"usage":raw}}),
        ]);
        let (base, _) = serve(body, "text/event-stream").await;
        let model = create_provider(
            "groq",
            PresetSettings {
                api_key: Some(Resolvable::Value("test-key".into())),
                base_url: Some(format!("{base}/v1")),
                ..Default::default()
            },
        )
        .unwrap()
        .language_model("gemma2-9b-it")
        .unwrap();
        let parts = collect(model.as_ref(), CallOptions::new(vec![])).await;
        let usage = finish_usage(&parts).unwrap();
        assert_eq!(usage.input_tokens.total, input, "groq {raw}");
        assert_eq!(usage.input_tokens.no_cache, no_cache, "groq {raw}");
        assert_eq!(usage.output_tokens.total, output, "groq {raw}");
        assert_eq!(usage.output_tokens.text, text, "groq {raw}");
        assert_eq!(usage.input_tokens.cache_read, None, "groq {raw}");
        assert_eq!(usage.output_tokens.reasoning, None, "groq {raw}");
    }
}

/// Reasoning tokens are reported beside the completion total.
#[tokio::test]
async fn chat_reasoning_tokens_reach_the_usage() {
    let body = sse(&[
        json!({"choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":6,"completion_tokens_details":{"reasoning_tokens":2}}}),
    ]);
    let (base, _) = serve(body, "text/event-stream").await;
    let parts = collect(&chat_model(&base, "openai"), CallOptions::new(vec![])).await;
    let usage = finish_usage(&parts).unwrap();
    assert_eq!(usage.input_tokens.total, Some(3));
    assert_eq!(usage.output_tokens.total, Some(6));
    assert_eq!(usage.output_tokens.reasoning, Some(2));
    assert_eq!(usage.output_tokens.text, Some(4));
}

/// User file parts take each protocol's documented shape.
#[tokio::test]
async fn user_file_parts_take_their_protocol_shape() {
    let file = |data: FileData, media_type: &str, filename: Option<&str>| {
        UserPart::File(FilePart {
            data,
            media_type: media_type.into(),
            filename: filename.map(str::to_string),
            provider_options: None,
        })
    };
    let prompt = vec![LanguageModelMessage::User {
        content: vec![
            file(
                FileData::Data {
                    data: FileBytes::Binary(b"image".to_vec()),
                },
                "image/jpeg",
                Some("picture.jpg"),
            ),
            file(
                FileData::Data {
                    data: FileBytes::Base64("aW1hZ2U=".into()),
                },
                "image/png",
                None,
            ),
            file(
                FileData::Data {
                    data: FileBytes::Binary(b"document".to_vec()),
                },
                "application/pdf",
                Some("note.pdf"),
            ),
            file(
                FileData::Url {
                    url: "https://example.test/picture.png?signature=opaque".into(),
                    original_url: None,
                },
                "image/png",
                None,
            ),
            file(
                FileData::Url {
                    url: "gs://bucket/document.pdf".into(),
                    original_url: None,
                },
                "application/pdf",
                None,
            ),
        ],
        provider_options: None,
    }];
    let options = CallOptions::new(prompt);

    let body = sse(&[json!({"type":"response.completed","response":{"status":"completed"}})]);
    let (base, seen) = serve(body, "text/event-stream").await;
    let model =
        aimux_providers::openai::create_openai(aimux_providers::openai::OpenAIProviderSettings {
            base_url: Some(format!("{base}/v1")),
            api_key: Some("test-key".into()),
            ..Default::default()
        })
        .unwrap()
        .responses("gpt-5");
    let _ = collect(&model, options.clone()).await;
    let request = seen.lock().unwrap()[0].clone();
    assert_eq!(
        request["input"][0]["content"],
        json!([
            {"type":"input_image","image_url":"data:image/jpeg;base64,aW1hZ2U="},
            {"type":"input_image","image_url":"data:image/png;base64,aW1hZ2U="},
            {"type":"input_file","filename":"note.pdf","file_data":"data:application/pdf;base64,ZG9jdW1lbnQ="},
            {"type":"input_image","image_url":"https://example.test/picture.png?signature=opaque"},
            {"type":"input_file","file_url":"gs://bucket/document.pdf"},
        ])
    );

    let body = b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[]},\"finishReason\":\"STOP\"}]}\n\n".to_vec();
    let (base, seen) = serve(body, "text/event-stream").await;
    let model =
        aimux_providers::google::create_google(aimux_providers::google::GoogleProviderSettings {
            base_url: Some(format!("{base}/v1beta")),
            api_key: Some("test-key".into()),
            ..Default::default()
        })
        .unwrap()
        .chat("gemini-test");
    let _ = collect(&model, options).await;
    let request = seen.lock().unwrap()[0].clone();
    assert_eq!(
        request["contents"][0]["parts"],
        json!([
            {"inlineData":{"mimeType":"image/jpeg","data":"aW1hZ2U="}},
            {"inlineData":{"mimeType":"image/png","data":"aW1hZ2U="}},
            {"inlineData":{"mimeType":"application/pdf","data":"ZG9jdW1lbnQ="}},
            {"fileData":{"mimeType":"image/png","fileUri":"https://example.test/picture.png?signature=opaque"}},
            {"fileData":{"mimeType":"application/pdf","fileUri":"gs://bucket/document.pdf"}},
        ])
    );
}

/// Corrupt, exceptional, truncated and non-JSON Converse frames are rejected
/// without a successful finish or a tool call.
#[tokio::test]
async fn malformed_converse_frames_never_finish_successfully() {
    let mut corrupt = frame(
        "contentBlockDelta",
        json!({"contentBlockIndex":0,"delta":{"text":"bad"}}),
    );
    *corrupt.last_mut().unwrap() ^= 1;
    let exception = encode_message(
        "exception",
        "modelStreamErrorException",
        "{\"message\":\"failed\"}",
    );
    let not_json = encode_message("event", "contentBlockDelta", "not-json");
    let truncated = frame("messageStop", json!({"stopReason":"end_turn"}))[..12].to_vec();
    let start = frame("messageStart", json!({"role":"assistant"}));
    for (case, data) in [
        ("corrupt", [start.clone(), corrupt].concat()),
        ("exception", [start.clone(), exception].concat()),
        ("not-json", [start.clone(), not_json].concat()),
        ("truncated", [start.clone(), truncated].concat()),
    ] {
        let (base, _) = serve(data, "application/vnd.amazon.eventstream").await;
        let provider = create_amazon_bedrock(AmazonBedrockProviderSettings {
            api_key: Some(Resolvable::Value("test-key".into())),
            region: Some("us-east-1".into()),
            base_url: Some(base),
            ..Default::default()
        })
        .unwrap();
        let parts = collect(&provider.chat("anthropic.test"), CallOptions::new(vec![])).await;
        assert!(
            parts
                .iter()
                .any(|part| matches!(part, Err(_) | Ok(StreamPart::Error { .. }))),
            "{case}: {parts:?}"
        );
        assert!(
            !parts.iter().any(|part| matches!(
                part,
                Ok(StreamPart::Finish { finish_reason, .. })
                    if finish_reason.unified != FinishReasonUnified::Error
            )),
            "{case}: {parts:?}"
        );
        assert!(
            !parts
                .iter()
                .any(|part| matches!(part, Ok(StreamPart::ToolCall(_)))),
            "{case}: {parts:?}"
        );
    }
}

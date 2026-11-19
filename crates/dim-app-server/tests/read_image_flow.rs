//! #656, #687: workspace reads retain images in history; conversations send
//! references even after user and tool images accumulate.
#![cfg(unix)]

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    routing::post,
};
use dim_agent_capabilities::{
    defaults,
    history::HistoryRenderer,
    host::*,
    host_platform::{HostPlatform, ShellStatus},
    workspace_tools::{self, WorkspaceToolsConfig},
};
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::{testing::MinimalHost, *};
use dim_agent_types::*;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

fn home() -> PathBuf {
    let directory = std::env::temp_dir().join(format!("dim-read-image-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(directory.join("work")).unwrap();
    directory.canonicalize().unwrap()
}

fn selection(model_id: &str, vision: bool) -> ModelSelection {
    let target = ModelId {
        provider_id: "local".into(),
        model_id: model_id.into(),
    };
    ModelSelection {
        provider_id: target.provider_id.clone(),
        model_id: target.model_id.clone(),
        capability_snapshot: Some(ModelCapabilitySnapshot {
            target,
            capabilities: ModelCapabilities {
                vision,
                ..ModelCapabilities::with_limits(128_000, 16_384)
            },
            reasoning: None,
            service_tier: None,
        }),
        config: None,
        metadata: None,
    }
}

const HISTORY_BODY_LIMIT: usize = 64 * 1024;
const HISTORY_IMAGE_LIMIT: usize = 2;
const HISTORY_CALLS: [(&str, &str); 2] =
    [("read-first", "first.gif"), ("read-second", "second.gif")];

fn history_image(seed: u8) -> Vec<u8> {
    const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00";
    const COMMENT_EXTENSION: [u8; 2] = [0x21, 0xfe];
    const TRAILER: u8 = 0x3b;
    let mut image = GIF.to_vec();
    image.extend(COMMENT_EXTENSION);
    for _ in 0..80 {
        image.push(255);
        image.extend([seed; 255]);
    }
    image.extend([0, TRAILER]);
    image
}

async fn history_completion(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    Json(request): Json<Value>,
) -> impl axum::response::IntoResponse {
    use axum::response::IntoResponse as _;
    use serde::Serialize;

    let image_count = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "image_url")
        .count();
    if image_count > HISTORY_IMAGE_LIMIT {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }

    #[derive(Serialize)]
    struct Chunk {
        id: &'static str,
        choices: [Choice; 1],
    }
    #[derive(Serialize)]
    struct Choice {
        index: usize,
        delta: Delta,
        finish_reason: Option<&'static str>,
    }
    #[derive(Serialize)]
    #[serde(untagged)]
    enum Delta {
        Batch { tool_calls: Vec<Call> },
        Text { content: &'static str },
        End {},
    }
    #[derive(Serialize)]
    struct Call {
        index: usize,
        id: &'static str,
        #[serde(rename = "type")]
        kind: &'static str,
        function: Function,
    }
    #[derive(Serialize)]
    struct Function {
        name: &'static str,
        arguments: String,
    }
    #[derive(Serialize)]
    struct ReadArgs {
        path: &'static str,
    }

    let request_number = {
        let mut requests = requests.lock().unwrap();
        requests.push(request);
        requests.len()
    };
    let (delta, finish) = if request_number == 3 {
        let tool_calls = HISTORY_CALLS
            .iter()
            .enumerate()
            .map(|(index, &(id, path))| Call {
                index,
                id,
                kind: "function",
                function: Function {
                    name: "read",
                    arguments: serde_json::to_string(&ReadArgs { path }).unwrap(),
                },
            })
            .collect();
        (Delta::Batch { tool_calls }, "tool_calls")
    } else {
        let content = match request_number {
            4 => "Read both images.",
            5 => "Image history is still available.",
            _ => "Image received.",
        };
        (Delta::Text { content }, "stop")
    };
    let event = |delta, finish_reason| {
        Event::default()
            .json_data(Chunk {
                id: "history-flow",
                choices: [Choice {
                    index: 0,
                    delta,
                    finish_reason,
                }],
            })
            .unwrap()
    };
    Sse::new(futures_util::stream::iter([
        Ok::<_, Infallible>(event(delta, None)),
        Ok(event(Delta::End {}, Some(finish))),
        Ok(Event::default().data("[DONE]")),
    ]))
    .into_response()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accumulated_user_and_read_images_do_not_block_native_conversation() {
    use base64::Engine as _;
    use dim_agent_aimux::{
        AimuxModels, AimuxPlugin, ConfiguredModelsPlugin,
        config::{ModelConfigFile, ModelEntry, ProviderConfig},
    };
    use sha2::{Digest, Sha256};
    use std::time::Duration;

    let home = tempfile::tempdir().unwrap();
    let work = home.path().canonicalize().unwrap();
    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "READ_IMAGE_FLOW",
        reason: "Reading the fixture runs no command".into(),
    })
    .unwrap();
    platform.home = work.clone();
    let images = (*b"abcd").map(history_image);
    let encoded = images
        .each_ref()
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes));
    let references = images.each_ref().map(|bytes| {
        let digest = format!("{:x}", Sha256::digest(bytes));
        format!("img:{}", &digest[..16])
    });
    assert!(images.len() > HISTORY_IMAGE_LIMIT);
    let user_bytes = encoded[..2].iter().map(String::len).sum::<usize>();
    assert!(user_bytes < HISTORY_BODY_LIMIT);
    assert!(encoded.iter().map(String::len).sum::<usize>() > HISTORY_BODY_LIMIT);
    for ((_, path), bytes) in HISTORY_CALLS.iter().zip(&images[2..]) {
        std::fs::write(work.join(path), bytes).unwrap();
    }

    let requests: Arc<Mutex<Vec<Value>>> = Default::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(history_completion))
        .layer(DefaultBodyLimit::max(HISTORY_BODY_LIMIT))
        .with_state(requests.clone());
    let endpoint = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let vision = selection("vision", true);
    let text = selection("text-only", false);
    let models = Arc::new(
        AimuxModels::from_config(ModelConfigFile {
            default_model: ModelId {
                provider_id: vision.provider_id.clone(),
                model_id: vision.model_id.clone(),
            },
            providers: vec![ProviderConfig {
                id: vision.provider_id.clone(),
                kind: ModelProtocol::OpenaiCompatible,
                api_key_env: None,
                base_url,
                headers: Default::default(),
                compatible: None,
                models: [&vision, &text]
                    .into_iter()
                    .map(|model| ModelEntry {
                        id: model.model_id.clone(),
                        context_window: Some(128_000),
                        max_output_tokens: Some(16_384),
                        config: ModelConfig {
                            max_tokens: Some(128.0),
                            ..Default::default()
                        },
                    })
                    .collect(),
            }],
        })
        .unwrap(),
    );
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_with_process_model(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .plugin(|_| Ok(Box::new(AimuxPlugin::default())))
        .plugin(move |_| Ok(Box::new(ConfiguredModelsPlugin(models.clone()))));
    dim_agent_aimux::install(&mut builder);
    workspace_tools::install(
        &mut builder,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: work.join("command-output"),
        },
    );
    let agent = MinimalHost::start_with(&builder, |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(platform))?;
        Ok(())
    })
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig {
            cwd: work.to_str().unwrap().into(),
            tools: Some(vec!["read".into()]),
            model: vision,
            permission_policy: PermissionPolicy {
                preset: PermissionPreset::FullAccess,
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();

    let image_inputs = encoded[..2].iter().map(|data| InputMessageData {
        user: Some(UserInput {
            content: vec![ContentBlock::Image {
                source: ImageSource {
                    file_id: None,
                    source_type: ImageSourceType::Base64,
                    media_type: "image/gif".into(),
                    data: Some(data.clone()),
                    url: None,
                },
            }],
        }),
        ..Default::default()
    });
    let text_inputs = [
        "Read the two workspace images.",
        "Use the images from the earlier turns.",
    ]
    .map(|text| InputMessageData::text(text, Some(InputMode::Queue)));
    for (index, input) in image_inputs.chain(text_inputs).enumerate() {
        if index == 3 {
            session
                .configure(
                    ConfigureUpdate {
                        model: Some(text.clone()),
                        ..Default::default()
                    },
                    "main",
                )
                .await
                .unwrap();
        }
        session.input(input, "main").await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
    }

    let history = chain_events(&session, "main", ..).await.unwrap();
    let stored_results: Vec<_> = history
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(&data.results),
            _ => None,
        })
        .flatten()
        .collect();
    for ((id, _), bytes) in HISTORY_CALLS.iter().zip(&encoded[2..]) {
        let result = stored_results
            .iter()
            .find(|result| result.call_id == *id)
            .unwrap();
        assert!(
            matches!(&result.result.content, ToolContent::Blocks(blocks)
            if blocks.iter().any(|block| matches!(block, ContentBlock::Image { source }
                if source.data.as_ref() == Some(bytes)))),
            "the timeline keeps the image for {id}"
        );
    }

    let turns: Vec<_> = history
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::TurnEnd(data) => Some(&data.outcome),
            _ => None,
        })
        .collect();
    assert_eq!(turns.len(), 4);
    assert!(
        turns
            .iter()
            .all(|outcome| matches!(outcome, TurnOutcome::Completed { .. })),
        "{turns:?}"
    );
    let replies: String = history
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Text(data) => Some(data.delta.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        replies.ends_with("Read both images.Image history is still available."),
        "{replies}"
    );

    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        for request in requests.iter() {
            let body = request.to_string();
            assert!(
                !body.contains("\"image_url\""),
                "chat requests must not carry pixels"
            );
            assert!(!encoded.iter().any(|image| body.contains(image)));
        }
        for (request, model) in requests[3..].iter().zip(["vision", "text-only"]) {
            assert_eq!(request["model"], model);
            let messages = request["messages"].as_array().unwrap();
            for reference in &references[..2] {
                assert!(
                    messages.iter().any(|message| message["role"] == "user"
                        && message["content"].to_string().contains(reference)),
                    "missing user image {reference}"
                );
            }
            let (assistant_index, assistant) = messages
                .iter()
                .enumerate()
                .find(|(_, message)| {
                    message["role"] == "assistant" && message["tool_calls"].is_array()
                })
                .expect("the assistant read batch is in the history");
            let calls = assistant["tool_calls"].as_array().unwrap();
            assert_eq!(calls.len(), HISTORY_CALLS.len());
            let results: Vec<_> = messages[assistant_index + 1..]
                .iter()
                .take_while(|message| message["role"] == "tool")
                .collect();
            assert_eq!(results.len(), HISTORY_CALLS.len());
            for ((id, path), reference) in HISTORY_CALLS.iter().zip(&references[2..]) {
                assert!(
                    calls
                        .iter()
                        .any(|call| call["id"] == *id && call["function"]["name"] == "read")
                );
                let result = results
                    .iter()
                    .find(|result| result["tool_call_id"] == *id)
                    .unwrap_or_else(|| panic!("missing result paired with {id}"));
                let content = result["content"].as_str().unwrap();
                assert!(
                    content.contains(reference)
                        && content.contains(&format!("Read image: {path} (1x1 px).")),
                    "{id}: {content}"
                );
            }
        }
    }

    agent.close().await.unwrap();
    endpoint.abort();
    assert!(endpoint.await.unwrap_err().is_cancelled());
}

/// Captures what WorkspaceTools registers, as SessionStore's `sessionFiles`
/// service would receive it.
struct RecordedOutputs(Arc<Mutex<Vec<PathBuf>>>);
impl SessionFilesService for RecordedOutputs {
    fn put(
        &self,
        _: String,
        _: String,
        _: Vec<u8>,
        _: CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<StoredSessionFile, SessionFileError>>
                + Send
                + '_,
        >,
    > {
        unreachable!("this flow stores no Session file")
    }
    fn register(
        &self,
        _: String,
        path: PathBuf,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), SessionFileError>> + Send + '_>> {
        self.0.lock().unwrap().push(path);
        Box::pin(async { Ok(()) })
    }
    fn owns(
        &self,
        _: String,
        _: PathBuf,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<bool, SessionFileError>> + Send + '_>>
    {
        Box::pin(async { Ok(false) })
    }
    fn owned_paths(
        &self,
        _: String,
    ) -> Pin<
        Box<dyn Future<Output = std::result::Result<Vec<PathBuf>, SessionFileError>> + Send + '_>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// ADR 013 D2: an offloaded `read` result registers its path before the
/// result reaches the model, through the Session part's process service.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_offloaded_read_result_registers_its_output_path() {
    let home = home();
    let work = home.join("work");
    std::fs::write(
        work.join("large.txt"),
        format!("{}\n", "x".repeat(500)).repeat(100),
    )
    .unwrap();

    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "OFFLOAD_FLOW",
        reason: "Reading a fixture runs no command".into(),
    })
    .unwrap();
    platform.home = home.clone();
    let registered = Arc::new(Mutex::new(Vec::new()));
    let owner = Arc::new(SessionFiles(Arc::new(RecordedOutputs(registered.clone()))));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    workspace_tools::install(
        &mut builder,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: home.join("command-output"),
        },
    );
    let agent = MinimalHost::start_with(&builder, move |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(platform))?;
        root.provide_as::<SessionFiles>(owner)?;
        Ok(())
    })
    .await
    .unwrap();

    let session = agent
        .create_session(SessionConfig {
            cwd: work.to_string_lossy().into_owned(),
            tools: Some(vec!["read".into()]),
            permission_policy: PermissionPolicy {
                preset: PermissionPreset::FullAccess,
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();
    session
        .submit(
            format!("tool:read {}", json!({ "path": "large.txt" })),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let history = chain_events(&session, "main", ..).await.unwrap();
    let output_path = history
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => data.results[0]
                .result
                .structured_content
                .as_ref()?
                .get("outputPath")?
                .as_str()
                .map(PathBuf::from),
            _ => None,
        })
        .expect("the read result names its offloaded output");
    assert!(output_path.exists(), "{}", output_path.display());
    assert_eq!(
        registered.lock().unwrap().as_slice(),
        std::slice::from_ref(&output_path)
    );

    session.close().await.unwrap();
    agent.close().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}

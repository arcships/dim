//! #656: `read` returns an image file as an image block, and the request the
//! model receives carries either that image or — for a model without image
//! input — the placeholder #654's rule puts in its place.
#![cfg(unix)]

use dim_agent_capabilities::{
    adapters::EchoModel,
    defaults,
    history::HistoryRenderer,
    host::*,
    host_platform::{HostPlatform, ShellStatus},
    workspace_tools::{self, WorkspaceToolsConfig},
};
use dim_agent_runtime::{testing::MinimalHost, *};
use dim_agent_types::*;
use serde_json::json;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

const PLACEHOLDER: &str = "[Image omitted: the current model does not accept images.]";
const LINE: &str = "Read image: screen.png (2x2 px).";
/// A 2x2 PNG (left column red, right column blue): its content, not its
/// extension, is what `read` recognizes.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR42mP4zwAE/xkgFAAb8gP9PpddpAAAAABJRU5ErkJggg==";

/// `EchoModel` with every request kept, so the test reads what the model got.
struct Recording {
    requests: Arc<Mutex<Vec<Arc<ModelRequest>>>>,
}
impl ModelDriver for Recording {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.requests.lock().unwrap().push(request.clone());
        EchoModel.invoke(request, connection, cancel)
    }
}

fn home() -> PathBuf {
    let directory = std::env::temp_dir().join(format!("dim-read-image-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(directory.join("work")).unwrap();
    directory.canonicalize().unwrap()
}

fn snapshot(vision: bool) -> ModelCapabilitySnapshot {
    ModelCapabilitySnapshot {
        target: ModelId {
            provider_id: "local".into(),
            model_id: "test-model".into(),
        },
        capabilities: ModelCapabilities {
            vision,
            ..ModelCapabilities::with_limits(128_000, 16_384)
        },
        reasoning: None,
        service_tier: None,
    }
}

fn stored_image(history: &[Arc<AgentEvent>]) -> bool {
    history.iter().any(|event| match &event.kind {
        EventKind::ToolResult(data) => matches!(
            &data.results[0].result.content,
            ToolContent::Blocks(blocks)
                if blocks.iter().any(|block| matches!(block, ContentBlock::Image { .. }))
        ),
        _ => false,
    })
}

/// The tool-result part of a request's messages, if it has one.
fn tool_result_part(request: &ModelRequest) -> Option<&MessagePart> {
    request.messages.iter().find_map(|message| {
        let MessageContent::Parts(parts) = &message.content else {
            return None;
        };
        parts
            .iter()
            .find(|part| matches!(part, MessagePart::ToolResult { .. }))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_image_reaches_a_vision_model_and_becomes_text_for_others() {
    let home = home();
    let work = home.join("work");
    std::fs::write(work.join("screen.png"), {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(PNG)
            .unwrap()
    })
    .unwrap();

    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "READ_IMAGE_FLOW",
        reason: "Reading the fixture runs no command".into(),
    })
    .unwrap();
    platform.home = home.clone();
    let requests: Arc<Mutex<Vec<Arc<ModelRequest>>>> = Default::default();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts_with_model(
        &mut builder,
        HistoryRenderer::default(),
        Arc::new(Recording {
            requests: requests.clone(),
        }),
    );
    workspace_tools::install(
        &mut builder,
        WorkspaceToolsConfig {
            output_root: home.join("command-output"),
        },
    );
    let agent = MinimalHost::start_with(&builder, |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(platform))?;
        Ok(())
    })
    .await
    .unwrap();

    for vision in [true, false] {
        let before = requests.lock().unwrap().len();
        let session = agent
            .create_session(SessionConfig {
                cwd: work.to_string_lossy().into_owned(),
                tools: Some(vec!["read".into()]),
                model: ModelSelection {
                    provider_id: "local".into(),
                    model_id: "test-model".into(),
                    capability_snapshot: Some(snapshot(vision)),
                    config: None,
                    metadata: None,
                },
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
                format!("tool:read {}", json!({ "path": "screen.png" })),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let history = session.history("main", None).await.unwrap();
        assert!(
            stored_image(&history),
            "vision={vision}: the stored tool result keeps the image"
        );

        let mut sent_image = false;
        let mut sent_placeholder = false;
        let mut sent_line = false;
        for request in requests.lock().unwrap().iter().skip(before) {
            let Some(MessagePart::ToolResult {
                content: ToolContent::Blocks(blocks),
                ..
            }) = tool_result_part(request)
            else {
                continue;
            };
            for block in blocks {
                match block {
                    ContentBlock::Image { .. } => sent_image = true,
                    ContentBlock::Text { text } if text == PLACEHOLDER => sent_placeholder = true,
                    ContentBlock::Text { text } if text == LINE => sent_line = true,
                    _ => {}
                }
            }
        }
        assert!(sent_line, "vision={vision}: the request carries the line");
        assert_eq!(
            sent_image, vision,
            "vision={vision}: only a model that takes images receives one"
        );
        assert_eq!(
            sent_placeholder, !vision,
            "vision={vision}: only a model without image input gets the placeholder"
        );
        session.close().await.unwrap();
    }
    agent.close().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
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
    let owner = Arc::new(SessionFilesHost(Arc::new(RecordedOutputs(
        registered.clone(),
    ))));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    workspace_tools::install(
        &mut builder,
        WorkspaceToolsConfig {
            output_root: home.join("command-output"),
        },
    );
    let agent = MinimalHost::start_with(&builder, move |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(platform))?;
        root.provide_as::<SessionFilesHost>(owner)?;
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
    let history = session.history("main", None).await.unwrap();
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

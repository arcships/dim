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
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::host::CtxHost;
use serde_json::json;
use std::{
    path::PathBuf,
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
            ..Default::default()
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

    let root = rutis::Ctx::root().unwrap();
    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "READ_IMAGE_FLOW",
        reason: "Reading the fixture runs no command".into(),
    })
    .unwrap();
    platform.home = home.clone();
    root.provide_as::<HostPlatform>(Arc::new(platform)).unwrap();
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
    let agent = builder.build_in(&root);
    agent.start().await.unwrap();

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
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(&home).unwrap();
}

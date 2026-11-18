use super::*;
use dim_agent_capabilities::{
    adapters::EchoModel,
    defaults,
    host::{HostFuture, ModelChunk, ModelConnection, ModelDriver, ModelStream},
    recall::{self, testing::Narrative},
};
use futures_util::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct SplitReply;
impl ModelDriver for SplitReply {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let stream = EchoModel.invoke(request, connection, cancel).await?;
            Ok(Box::pin(stream.flat_map(|chunk| {
                let chunks = match chunk {
                    Ok(ModelChunk::Text(text)) if text.contains("UNIQUENEEDLE") => {
                        let at = text.find("NEEDLE").unwrap();
                        vec![
                            Ok(ModelChunk::Text(text[..at].into())),
                            Ok(ModelChunk::Text(text[at..].into())),
                        ]
                    }
                    chunk => vec![chunk],
                };
                futures_util::stream::iter(chunks)
            })) as ModelStream)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_recall_verifies_indexed_candidates_and_fallbacks_against_the_same_full_projection()
{
    tokio::time::timeout(Duration::from_secs(90), async {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts_with_model(
            &mut builder,
            HistoryRenderer::default(),
            Arc::new(SplitReply),
        );
        defaults::install(&mut builder);
        recall::install(&mut builder);
        builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()));
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig {
                tools: None,
                ..SessionConfig::default()
            })
            .await
            .unwrap();
        for message in [
            "first UNIQUENEEDLE",
            "a 中文 and İ message",
            "ordinary final message",
        ] {
            session
                .submit(message.into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
        }
        for arguments in [
            json!({"about":"uniqueneedle"}),
            json!({"about":"absentwordxyz"}),
            json!({"about":"un"}),
            json!({"about":"中文"}),
            json!({"about":"İ"}),
            json!({"about":"uniqueneedle","tool":"echo"}),
            json!({}),
        ] {
            store.record_query_statistics();
            session
                .submit(format!("tool:recall {arguments}"), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let statistics = store.take_query_statistics();
            let history = chain_events(&session, "main", ..).await.unwrap();
            let at = history
                .iter()
                .rposition(|event| matches!(event.kind, EventKind::ToolResult(_)))
                .unwrap_or_else(|| {
                    panic!(
                        "Recall returned no tool result for {arguments}: {:?}",
                        history
                            .iter()
                            .map(|event| event.kind.key())
                            .collect::<Vec<_>>()
                    )
                });
            let EventKind::ToolResult(data) = &history[at].kind else {
                unreachable!()
            };
            let actual = &data.results.last().unwrap().result;
            let calling = history[..at]
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    EventKind::TurnStart(data) => Some(data.turn_id.as_str()),
                    _ => None,
                })
                .unwrap();
            let expected =
                Narrative::new(&history[..at]).answer(calling, arguments.as_object().unwrap());
            assert_eq!(actual.content, expected.content, "{arguments}");
            assert_eq!(
                actual.is_error.unwrap_or(false),
                expected.is_error.unwrap_or(false),
                "{arguments}"
            );
            if arguments["about"] == "uniqueneedle" {
                assert!(
                    statistics
                        .iter()
                        .any(|statement| statement.plan.iter().any(|line| {
                            line.contains("timeline_text_fts")
                                && line.contains("VIRTUAL TABLE INDEX")
                        })),
                    "{statistics:?}"
                );
            }
        }
        agent.close().await.unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
    })
    .await
    .expect("SQLite Recall flow");
}

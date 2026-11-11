use super::*;
use dim_agent_capabilities::{defaults, prompt::Prompt, prompt_state::PromptSnapshot};
use dim_agent_runtime::{ConfigureOutcome, PromptSourceTarget};
use serde_json::Value;

fn builder(store: &Arc<SqliteTimelineStore>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_resume_reads_effective_elements_and_recovers_a_committed_adoption_once() {
    let mut costs = Vec::new();
    for size in [128usize, 2048] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let agent = MinimalHost::start(&builder(&store)).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let id = session.id().to_owned();
        let prompt = session.service::<Prompt>("main").unwrap();
        let scope = PromptScope::Branch {
            timeline: "main".into(),
        };
        let entries = (0..size)
            .map(|index| {
                let entry = PromptEntry {
                    source: format!("source-{index}"),
                    version: "1".into(),
                    body: format!("entry-{index}:{}", "x".repeat(256)),
                    scope: scope.clone(),
                    delivery: PromptDelivery::System,
                    order: index as i32,
                };
                (
                    PromptSourceTarget {
                        source: entry.source.clone(),
                        scope: scope.clone(),
                    },
                    Some(entry),
                )
            })
            .collect::<Vec<_>>();
        store.take_node_loads();
        assert_eq!(
            prompt
                .clone()
                .configure_loaded(
                    Some(ModelSelection {
                        provider_id: "test".into(),
                        model_id: "echo".into(),
                        capability_snapshot: None,
                        config: None,
                        metadata: None,
                    }),
                    entries.clone()
                )
                .await
                .unwrap(),
            ConfigureOutcome::Adopted
        );
        assert_eq!(
            store.take_node_loads().loads,
            0,
            "live adoption consumes dispatched facts without querying their array bodies again"
        );
        let revoked = entries
            .iter()
            .enumerate()
            .filter(|(index, _)| ![2, 7].contains(index))
            .map(|(_, (target, _))| (target.clone(), None))
            .collect();
        assert_eq!(
            prompt
                .clone()
                .configure_loaded(None, revoked)
                .await
                .unwrap(),
            ConfigureOutcome::Adopted
        );
        let expected = session.get::<PromptSnapshot>("main").unwrap().unwrap();
        assert_eq!(expected.entries.len(), 2);
        let connection = Connection::open(&database.0).unwrap();
        let array:String=connection.query_row("SELECT json_extract(body,'$.record.changes.id') FROM timeline_events WHERE session_id=?1 AND kind='prompt/changed' ORDER BY sequence LIMIT 1",[&id],|row|row.get(0)).unwrap();
        let recovered:String=connection.query_row("SELECT json_extract(body,'$.record.event.data') FROM timeline_events WHERE session_id=?1 AND kind='prompt-state/recovered' ORDER BY sequence DESC LIMIT 1",[&id],|row|row.get(0)).unwrap();
        let recovered: Value = serde_json::from_str(&recovered).unwrap();
        assert_eq!(recovered["state"]["entries"].as_array().unwrap().len(), 2);
        assert!(
            !recovered
                .to_string()
                .contains(&expected.entries.values().next().unwrap().body)
        );
        drop(connection);
        drop(prompt);
        agent.close().await.unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        let connection = Connection::open(&database.0).unwrap();
        connection.execute("UPDATE timeline_content SET body='{}' WHERE session_id=?1 AND kind='array-element' AND hash NOT IN (SELECT content_ref FROM timeline_array_elements WHERE session_id=?1 AND array_id=?2 AND element_index IN (2,7))",params![id,array]).unwrap();
        drop(connection);
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let agent = MinimalHost::start(&builder(&store)).await.unwrap();
        store.take_node_loads();
        store.record_query_statistics();
        let session = settled(
            "effective prompts restore without reading inactive elements",
            agent.restore_session(id.clone()),
        )
        .await
        .unwrap();
        let actual = session.get::<PromptSnapshot>("main").unwrap().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            store.take_node_loads().loads,
            2,
            "only the two current prompt elements are decoded"
        );
        costs.push(
            store
                .take_query_statistics()
                .iter()
                .map(|stat| stat.vm_steps)
                .sum::<i32>(),
        );
        let mut updated = actual.entries.values().next().unwrap().clone();
        updated.version = "2".into();
        updated.body = "updated after the saved result".into();
        let target = PromptSourceTarget {
            source: updated.source.clone(),
            scope: updated.scope.clone(),
        };
        let before = i64::try_from(session.sequence("main").unwrap()).unwrap();
        let connection = Connection::open(&database.0).unwrap();
        connection.execute_batch(&format!("CREATE TRIGGER fail_prompt_result BEFORE INSERT ON timeline_events WHEN NEW.session_id='{id}' AND NEW.kind='prompt-state/recovered' BEGIN SELECT RAISE(ABORT,'injected prompt result failure'); END;")).unwrap();
        drop(connection);
        let result = settled(
            "a failed result does not acknowledge the adoption",
            session
                .service::<Prompt>("main")
                .unwrap()
                .configure_loaded(None, vec![(target, Some(updated.clone()))]),
        )
        .await;
        assert!(result.is_err());
        let connection = Connection::open(&database.0).unwrap();
        assert_eq!(connection.query_row("SELECT count(*) FROM timeline_events WHERE session_id=?1 AND sequence>?2 AND kind='prompt/changed'",params![id,before],|row|row.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(connection.query_row("SELECT count(*) FROM timeline_events WHERE session_id=?1 AND sequence>?2 AND kind='prompt-state/recovered'",params![id,before],|row|row.get::<_,i64>(0)).unwrap(),0);
        connection
            .execute_batch("DROP TRIGGER fail_prompt_result")
            .unwrap();
        drop(connection);
        let _ = agent.close().await;
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let agent = MinimalHost::start(&builder(&store)).await.unwrap();
        let session = settled(
            "the saved prefix consumes the unfinished adoption",
            agent.restore_session(id.clone()),
        )
        .await
        .unwrap();
        let actual = session.get::<PromptSnapshot>("main").unwrap().unwrap();
        assert_eq!(actual.entries.len(), 2);
        assert_eq!(
            actual.entries[&(updated.source.clone(), scope.clone())],
            updated
        );
        let connection = Connection::open(&database.0).unwrap();
        assert_eq!(connection.query_row("SELECT count(*) FROM timeline_events WHERE session_id=?1 AND sequence>?2 AND kind='prompt/changed'",params![id,before],|row|row.get::<_,i64>(0)).unwrap(),1,"recovery does not repeat the committed configuration batch");
        drop(connection);
        agent.close().await.unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        let connection = Connection::open(&database.0).unwrap();
        connection.execute("UPDATE timeline_events SET body=json_set(body,'$.record.event.data.schema','prompt-state/v999') WHERE session_id=?1 AND sequence=(SELECT max(sequence) FROM timeline_events WHERE session_id=?1 AND kind='prompt-state/recovered')",[&id]).unwrap();
        drop(connection);
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let agent = MinimalHost::start(&builder(&store)).await.unwrap();
        let failure = settled(
            "unknown prompt schema fails without replay",
            agent.restore_session(id),
        )
        .await
        .err()
        .expect("newer recovery state is incompatible");
        assert!(
            failure.to_string().contains("unsupported recovery schema"),
            "{failure}"
        );
        let _ = agent.close().await;
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    assert!(
        costs[1] < costs[0] * 2,
        "restore query work must not grow with the obsolete prompt arrays: {costs:?}"
    );
}

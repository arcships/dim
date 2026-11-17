use super::*;
use dim_agent_capabilities::{defaults, methods::RpcError, prompt::Prompt};
use dim_agent_runtime::{ConfigureOutcome, PromptSourceTarget};
use dim_app_server::storage::migrate_prompt_references;
use std::path::Path;

fn builder(store: &Arc<SqliteTimelineStore>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    builder
}

/// A Session whose `main` adopted the same source twice, then lost every saved
/// reference: the history the guard demands an explicit migration for.
///
/// The two adoptions are what makes the first `prompt/changed` differ from the
/// last, which is the condition `Projection::load` refuses without a saved
/// reference; dropping the recovery rows leaves exactly the store a branch
/// written before `prompt-state/v2` has.
async fn legacy_session(database: &Path) -> String {
    let db = SqliteStorage::open(database.to_owned()).unwrap();
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
    let target = PromptSourceTarget {
        source: "domain-state".into(),
        scope: scope.clone(),
    };
    for version in ["first", "latest"] {
        let entry = PromptEntry {
            source: target.source.clone(),
            version: version.into(),
            body: format!("{version} domain state"),
            scope: scope.clone(),
            delivery: PromptDelivery::State,
            order: 0,
        };
        assert_eq!(
            prompt
                .clone()
                .configure_loaded(None, vec![(target.clone(), Some(entry))])
                .await
                .unwrap(),
            ConfigureOutcome::Adopted
        );
    }
    drop(prompt);
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(database).unwrap();
    let changed: i64 = connection
        .query_row(
            "SELECT count(*) FROM timeline_events WHERE session_id=?1 AND kind='prompt/changed'",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(changed >= 2, "the seed carries {changed} adoptions");
    connection
        .execute(
            "DELETE FROM timeline_events WHERE session_id=?1 AND kind='prompt-state/recovered'",
            [&id],
        )
        .unwrap();
    drop(connection);
    id
}

/// Restores `id` in a fresh Agent and returns its prompt state, or the error
/// the guard raised.
async fn resume(
    database: &Path,
    id: &str,
) -> dim_agent_runtime::Result<dim_agent_capabilities::prompt_state::PromptSnapshot> {
    let db = SqliteStorage::open(database.to_owned()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let agent = MinimalHost::start(&builder(&store)).await.unwrap();
    let result = settled("the branch settles", agent.restore_session(id.to_owned())).await;
    let snapshot = match result {
        Ok(session) => session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
            .unwrap(),
        Err(error) => {
            let _ = agent.close().await;
            store.close().await.unwrap();
            db.close().await.unwrap();
            return Err(error);
        }
    };
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    Ok(snapshot.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_legacy_branch_answers_its_migration_instruction_on_the_wire() {
    let database = Database::new();
    let id = legacy_session(&database.0).await;
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let agent = MinimalHost::start(&builder(&store)).await.unwrap();
    let failure = settled(
        "the guard refuses a branch with no saved references",
        agent.restore_session(id),
    )
    .await
    .err()
    .expect("legacy prompt adoption must require migration");
    assert!(
        matches!(&failure, AgentError::Framework(kind) if matches!(kind.as_ref(), rutis::host::ErrorKind::PluginFailed(source) if source.downcast_ref::<AgentError>().is_some())),
        "the refusal crosses the branch build as a wrapped AgentError: {failure:?}"
    );
    let wire = RpcError::from(failure);
    let app_error = wire
        .data
        .as_ref()
        .map(|data| data.app_error.as_str())
        .unwrap_or_default();
    assert_eq!(wire.code, -32000, "{wire:?}");
    assert_eq!(app_error, "session_invalid_input", "{wire:?}");
    assert!(
        wire.message.contains("dimcode --migrate-prompt-references"),
        "the client reads the command that shipped: {wire:?}"
    );
    let _ = agent.close().await;
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_offline_entry_migrates_once_and_then_reports_no_writes() {
    let database = Database::new();
    let id = legacy_session(&database.0).await;
    assert!(
        resume(&database.0, &id).await.is_err(),
        "the store starts in the state the guard refuses"
    );
    let first = migrate_prompt_references(database.0.clone(), id.clone(), "main".into())
        .await
        .unwrap();
    assert!(first.written, "the legacy history writes one saved result");
    let second = migrate_prompt_references(database.0.clone(), id.clone(), "main".into())
        .await
        .unwrap();
    assert!(
        !second.written,
        "a repeated run writes nothing and is not an error"
    );
    let snapshot = resume(&database.0, &id)
        .await
        .expect("the migrated branch resumes");
    assert_eq!(
        snapshot.entries[&(
            "domain-state".into(),
            PromptScope::Branch {
                timeline: "main".into()
            }
        )]
            .body,
        "latest domain state",
        "the migration keeps the latest adoption"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_offline_entry_refuses_a_held_store_and_an_unsupported_format() {
    let database = Database::new();
    let id = legacy_session(&database.0).await;
    let before = std::fs::metadata(&database.0).unwrap().len();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let failure = migrate_prompt_references(database.0.clone(), id.clone(), "main".into())
        .await
        .err()
        .expect("a running store owns the database");
    let message = failure.to_string();
    assert!(
        message.contains("another process owns the SQLite database"),
        "{message}"
    );
    assert!(
        message.contains("stop the daemon"),
        "the refusal says what to do: {message}"
    );
    db.close().await.unwrap();
    assert_eq!(
        std::fs::metadata(&database.0).unwrap().len(),
        before,
        "a refused run does not change the database"
    );
    let connection = Connection::open(&database.0).unwrap();
    // A store whose schema is not the current format's: an extra object is
    // enough for `known_format` to stop matching, and an older dimcode
    // leaves a shape this one refuses rather than upgrades.
    connection
        .execute_batch("CREATE TABLE legacy_marker (id TEXT)")
        .unwrap();
    drop(connection);
    let before = std::fs::metadata(&database.0).unwrap().len();
    let failure = migrate_prompt_references(database.0.clone(), id, "main".into())
        .await
        .err()
        .expect("an unsupported format is refused");
    let message = failure.to_string();
    assert!(
        message.contains("unsupported Timeline storage format"),
        "{message}"
    );
    let connection = Connection::open(&database.0).unwrap();
    let marker: i64 = connection
        .query_row("SELECT version FROM agent_store_format", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        marker,
        i64::from(STORE_FORMAT),
        "the refused run does not upgrade the store"
    );
    drop(connection);
    assert_eq!(
        std::fs::metadata(&database.0).unwrap().len(),
        before,
        "a refused format leaves the database as it was"
    );
}

/// The command the guard's message names, run as the released binary runs it:
/// the same store, migrated through `dimcode --migrate-prompt-references`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shipped_binary_migrates_what_its_error_told_the_user_to_migrate() {
    let database = Database::new();
    let id = legacy_session(&database.0).await;
    assert!(
        resume(&database.0, &id).await.is_err(),
        "the store starts in the state the guard refuses"
    );
    let run = |database: &Path, id: &str| {
        let database = database.to_owned();
        let id = id.to_owned();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
                .arg("--migrate-prompt-references")
                .arg("--database")
                .arg(&database)
                .arg("--session")
                .arg(&id)
                .arg("--branch")
                .arg("main")
                .output()
                .await
                .unwrap()
        }
    };
    let first = run(&database.0, &id).await;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let printed = String::from_utf8_lossy(&first.stdout);
    assert!(printed.contains("Wrote 1"), "{printed}");
    assert!(
        printed.contains(&id),
        "the report names the Session: {printed}"
    );
    assert!(printed.contains("branch main"), "{printed}");
    resume(&database.0, &id)
        .await
        .expect("the migrated branch resumes");
    let second = run(&database.0, &id).await;
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let printed = String::from_utf8_lossy(&second.stdout);
    assert!(
        printed.contains("Wrote 0"),
        "a repeated run reports no writes and still succeeds: {printed}"
    );
}

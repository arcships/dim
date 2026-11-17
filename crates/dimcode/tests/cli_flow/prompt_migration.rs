//! The legacy prompt-reference migration through the command the guard's own
//! error names (`docs/design/prompt-state-read.md` "显式旧引用迁移入口").
use super::*;
use dim_agent_capabilities::{defaults, prompt::Prompt, prompt_state::PromptSnapshot};
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, ConfigureOutcome, PromptSourceTarget, SessionConfig,
};
use dim_agent_types::{PromptDelivery, PromptEntry, PromptScope};
use std::{path::Path, sync::Arc};

fn builder(store: &Arc<dim_app_server::storage::SqliteTimelineStore>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    builder
}

/// The store of `database` twice: the concrete handle whose `close` this
/// flow calls, and the trait object an `AgentBuilder` takes.
fn open(
    database: &Path,
) -> (
    Arc<dim_app_server::storage::SqliteStorage>,
    Arc<dim_app_server::storage::SqliteTimelineStore>,
) {
    let storage = dim_app_server::storage::SqliteStorage::open(database.to_owned()).unwrap();
    let store = dim_app_server::storage::SqliteTimelineStore::new(storage.clone());
    (storage, store)
}

/// A Session whose `main` adopted one source twice and then lost every saved
/// reference: the history the guard demands an explicit migration for.
///
/// The two adoptions are what makes the first `prompt/changed` differ from the
/// last, which is the condition `Projection::load` refuses without a saved
/// reference; dropping the recovery rows leaves the store a branch written
/// before `prompt-state/v2` has.
async fn legacy_session(database: &Path) -> String {
    let (storage, store) = open(database);
    let agent = dim_agent_runtime::testing::MinimalHost::start(&builder(&store))
        .await
        .unwrap();
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
    storage.close().await.unwrap();
    let connection = rusqlite::Connection::open(database).unwrap();
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

/// Restores `id` into a fresh Agent and returns its prompt state, or the
/// error the guard raised.
async fn resume(database: &Path, id: &str) -> dim_agent_runtime::Result<PromptSnapshot> {
    let (storage, store) = open(database);
    let agent = dim_agent_runtime::testing::MinimalHost::start(&builder(&store))
        .await
        .unwrap();
    let restored = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        agent.restore_session(id.to_owned()),
    )
    .await
    .expect("the branch settles");
    let snapshot = match restored {
        Ok(session) => session.get::<PromptSnapshot>("main").unwrap(),
        Err(error) => {
            let _ = agent.close().await;
            store.close().await.unwrap();
            storage.close().await.unwrap();
            return Err(error);
        }
    };
    agent.close().await.unwrap();
    store.close().await.unwrap();
    storage.close().await.unwrap();
    Ok(snapshot.unwrap())
}

/// `dimcode --migrate-prompt-references ...` is the command the guard's error
/// prints, so it is the one this flow drives: the shipped executable, not the
/// App Server binary beside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dimcode_migrates_the_branch_its_error_told_the_user_to_migrate() {
    let home = Home::new();
    let database = home.0.join("rust/dimcode.sqlite");
    let id = legacy_session(&database).await;
    let blocked = resume(&database, &id)
        .await
        .expect_err("the store starts in the state the guard refuses");
    assert!(
        blocked.to_string().contains("explicit migration"),
        "the store is refused by the migration guard: {blocked}"
    );
    let run = |database: &Path, id: &str| {
        let database = database.to_owned();
        let id = id.to_owned();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_dimcode"))
                .arg("--migrate-prompt-references")
                .arg("--database")
                .arg(&database)
                .arg("--session")
                .arg(&id)
                .arg("--branch")
                .arg("main")
                .env("DIMCODE_UPDATE_CHECK", "0")
                .output()
                .await
                .unwrap()
        }
    };
    let first = run(&database, &id).await;
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
    let snapshot = resume(&database, &id)
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
    let second = run(&database, &id).await;
    assert!(
        second.status.success(),
        "a repeated run is not an error: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let printed = String::from_utf8_lossy(&second.stdout);
    assert!(
        printed.contains("Wrote 0"),
        "a repeated run reports no writes: {printed}"
    );
}

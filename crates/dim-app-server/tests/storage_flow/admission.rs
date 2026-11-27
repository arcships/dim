//! #2204: a new Session id admitted through `CreateSessionOptions::prepare`,
//! `createOwnedSession` or a fork's target is lowercase ASCII — one to
//! sixty-four characters of `[a-z0-9_-]`, and never a Windows reserved device
//! name — and is rejected with InvalidInput before anything is written. The
//! store refuses a write whose id differs from a stored one only by case:
//! the case variant is InvalidInput, while a byte-identical duplicate keeps
//! its Storage error. A fork refused before its copy starts writes nothing
//! and drops nothing, so a target byte-identical to a stored Session cannot
//! delete that Session's rows. Ids already stored keep their bytes:
//! restoration reads them as written, and deletion plus use as a fork source
//! are unchanged.

use super::*;

fn seed_owner(connection: &Connection) {
    connection
        .execute(
            "INSERT INTO agents(agent_id, profile, is_default) VALUES ('owner', ?1, 1)",
            [serde_json::to_string(&AgentProfile {
                name: "owner".into(),
                description: String::new(),
                system_prompt: String::new(),
                tools: Some(vec![]),
                model: None,
            })
            .unwrap()],
        )
        .unwrap();
}

/// A stored record written around every validation, the way a pre-#2204 or
/// external writer could leave it: uppercase letters and all.
fn seed_stored_session(connection: &Connection, session_id: &str) {
    let config = serde_json::to_string(&SessionConfig::default()).unwrap();
    connection
        .execute(
            "INSERT INTO agent_sessions (session_id, config, created_at_ms, origin) \
             VALUES (?1, ?2, 0, 'client')",
            params![session_id, config],
        )
        .unwrap();
}

fn stored_events(connection: &Connection, session_id: &str) -> Vec<String> {
    connection
        .prepare("SELECT sequence || ':' || body FROM timeline_events WHERE session_id=?1 ORDER BY sequence")
        .unwrap()
        .query_map([session_id], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn stored_rows(connection: &Connection, session_id: &str) -> i64 {
    connection
        .query_row(
            "SELECT (SELECT count(*) FROM agent_sessions WHERE session_id=?1) + \
             (SELECT count(*) FROM timeline_branches WHERE session_id=?1) + \
             (SELECT count(*) FROM timeline_events WHERE session_id=?1)",
            [session_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn prepared_creation_admits_only_lowercase_ascii_ids() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let connection = Connection::open(&database.0).unwrap();

    let overlong = "x".repeat(65);
    for id in [
        "Uppercase",
        "with space",
        "con",
        "com0",
        "lpt9",
        "unicode-é",
        overlong.as_str(),
    ] {
        let prepared_store = store.clone();
        let options =
            dim_agent_runtime::CreateSessionOptions::default().prepare(id, move |id: String| {
                Box::pin(async move {
                    prepared_store
                        .create_session(&id, &SessionConfig::default())
                        .await
                })
            });
        let result = agent
            .create_session(SessionConfig::default(), options)
            .await;
        assert!(
            matches!(result, Err(AgentError::InvalidInput(_))),
            "prepared creation with id {id:?} must be rejected with InvalidInput, got {:?}",
            result.err()
        );
        assert_eq!(
            stored_rows(&connection, id),
            0,
            "nothing of {id:?} may be written"
        );
    }

    let prepared_store = store.clone();
    let options = dim_agent_runtime::CreateSessionOptions::default().prepare(
        "valid-id_1",
        move |id: String| {
            Box::pin(async move {
                prepared_store
                    .create_session(&id, &SessionConfig::default())
                    .await
            })
        },
    );
    agent
        .create_session(SessionConfig::default(), options)
        .await
        .unwrap();

    drop(connection);
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn owned_creation_admits_only_lowercase_ascii_ids() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let root = rutis::Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![directory.clone()])))
        .await
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = dim_app_server::storage::SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    seed_owner(&connection);

    let overlong = "x".repeat(65);
    for id in [
        "Uppercase",
        "with space",
        "con",
        "com0",
        "lpt9",
        "unicode-é",
        overlong.as_str(),
    ] {
        let result = sessions
            .create_owned_session(
                id,
                &SessionConfig::default(),
                "owner",
                2,
                None,
                SessionOrigin::Client,
                None,
            )
            .await;
        assert!(
            matches!(result, Err(AgentError::InvalidInput(_))),
            "owned creation with id {id:?} must be rejected with InvalidInput, got {result:?}"
        );
        assert_eq!(
            stored_rows(&connection, id),
            0,
            "nothing of {id:?} may be written"
        );
    }

    sessions
        .create_owned_session(
            "valid-id_1",
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await
        .unwrap();

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn owned_creation_rejects_case_variants_and_keeps_storage_error_for_byte_duplicates() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let root = rutis::Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![directory.clone()])))
        .await
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = dim_app_server::storage::SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    seed_owner(&connection);
    seed_stored_session(&connection, "Legacy-1");
    seed_stored_session(&connection, "stored-1");

    let variant = sessions
        .create_owned_session(
            "legacy-1",
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await;
    assert!(
        matches!(variant, Err(AgentError::InvalidInput(_))),
        "an id differing only by case must be InvalidInput, got {variant:?}"
    );
    // A byte-identical duplicate is only reachable under the admission rule
    // when the stored id is itself conforming: the uppercase id above would
    // already fail admission. The colliding write keeps the duplicate's
    // Storage error; only the case variant is InvalidInput.
    let duplicate = sessions
        .create_owned_session(
            "stored-1",
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await;
    assert!(
        matches!(duplicate, Err(AgentError::Storage(_))),
        "a byte-identical duplicate must keep its Storage error, got {duplicate:?}"
    );
    assert_eq!(
        stored_rows(&connection, "legacy-1"),
        0,
        "the refused variant writes nothing"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM agent_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2,
        "the stored records are untouched"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_target_outside_the_admission_rule_or_in_case_collision_writes_nothing() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let root = rutis::Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![directory.clone()])))
        .await
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = dim_app_server::storage::SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    seed_stored_session(&connection, "Legacy-2");
    store
        .create_session("source", &SessionConfig::default())
        .await
        .unwrap();
    append(&store, key("source", "main"), vec![text(1, "source")])
        .await
        .unwrap();
    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();

    for target in ["Uppercase", "con"] {
        let result = sessions.fork_session("source", target, 1, &config).await;
        assert!(
            matches!(result, Err(AgentError::InvalidInput(_))),
            "fork target {target:?} must be rejected with InvalidInput, got {result:?}"
        );
        assert_eq!(
            stored_rows(&connection, target),
            0,
            "a refused fork target writes nothing"
        );
    }

    let collision = sessions
        .fork_session("source", "legacy-2", 1, &config)
        .await;
    assert!(
        matches!(collision, Err(AgentError::InvalidInput(_))),
        "a fork target differing only by case must be InvalidInput, got {collision:?}"
    );
    assert_eq!(
        stored_rows(&connection, "legacy-2"),
        0,
        "the collision refusal writes nothing under the refused id"
    );
    assert_eq!(
        stored_rows(&connection, "Legacy-2"),
        1,
        "the seeded uppercase session row is untouched"
    );
    assert!(
        stored_events(&connection, "legacy-2").is_empty(),
        "no Timeline rows may be copied into the colliding id"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// #2204's worst case: a fork whose target is byte-identical to a stored
/// Session. The copy's final insert cannot succeed, and the copy's cleanup
/// drops every row of the id — including the Session that already owned it,
/// before the fix. The refusal must happen before the first copy chunk, so
/// nothing is written and no cleanup runs.
#[tokio::test]
async fn fork_with_a_byte_identical_target_is_refused_without_touching_the_existing_session() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let root = rutis::Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![directory.clone()])))
        .await
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = dim_app_server::storage::SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("source", &SessionConfig::default())
        .await
        .unwrap();
    append(&store, key("source", "main"), vec![text(1, "source")])
        .await
        .unwrap();
    store
        .create_session("victim", &SessionConfig::default())
        .await
        .unwrap();
    append(&store, key("victim", "main"), vec![text(1, "victim")])
        .await
        .unwrap();

    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();
    let result = sessions.fork_session("source", "victim", 1, &config).await;
    assert!(
        matches!(result, Err(AgentError::Storage(_))),
        "a byte-identical fork target must keep its Storage error, got {result:?}"
    );

    let connection = Connection::open(&database.0).unwrap();
    assert!(
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id='victim')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap(),
        "the existing Session's row must survive the refused fork"
    );
    assert_eq!(
        stored_events(&connection, "victim").len(),
        1,
        "the existing Session's Timeline rows must survive the refused fork"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// Ids stored before the admission rule are neither migrated nor rewritten:
/// the raw store still writes them as given, an uppercase source forks into
/// a conforming target, and deletion removes it as before. This pins behavior
/// the change must not alter.
#[tokio::test]
async fn stored_uppercase_ids_remain_operable() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let root = rutis::Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![directory.clone()])))
        .await
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = dim_app_server::storage::SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("Legacy-9", &SessionConfig::default())
        .await
        .unwrap();
    append(
        &store,
        key("Legacy-9", "main"),
        vec![text(1, "first"), text(2, "second")],
    )
    .await
    .unwrap();

    let config = sessions
        .session_config_text("Legacy-9")
        .await
        .unwrap()
        .unwrap();
    sessions
        .fork_session("Legacy-9", "valid-fork", 2, &config)
        .await
        .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert!(
        !stored_events(&connection, "valid-fork").is_empty(),
        "the uppercase source's rows are copied into the fork"
    );

    assert!(sessions.delete_session("Legacy-9").await.unwrap());
    assert!(
        connection
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id='Legacy-9')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap(),
        "deletion removes the uppercase id as before"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// The collision check is the indexed lookup the maintainer decision requires:
/// the query the write entries run resolves through the covering derived
/// index instead of scanning the table.
#[tokio::test]
async fn case_collision_check_uses_the_derived_index() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let connection = Connection::open(&database.0).unwrap();
    let plan: Vec<String> = connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT session_id FROM agent_sessions \
             WHERE session_id = ?1 COLLATE NOCASE AND session_id <> ?1",
        )
        .unwrap()
        .query_map(["candidate"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.contains("agent_sessions_session_id_nocase")),
        "the collision check must use the covering index, plan: {plan:?}"
    );

    drop(connection);
    db.close().await.unwrap();
}

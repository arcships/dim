//! #2204: a new Session id admitted through `CreateSessionOptions::prepare`,
//! `createOwnedSession` or a fork's target is lowercase ASCII — one to
//! sixty-four characters of `[a-z0-9_-]`, and never a Windows reserved device
//! name — and is rejected with InvalidInput before anything is written. The
//! store refuses a write whose id differs from a stored one only by case,
//! and a write whose id would share an output directory with a stored one
//! under a supported filesystem's identity rules — Unicode folding pairs,
//! default-ignorable code points, the Windows separators, trailing dots and
//! spaces and Unicode mappings, and the 8.3 short names. Both refusals are
//! InvalidInput, while a byte-identical duplicate keeps its Storage error. A
//! fork refused before its copy starts writes nothing and drops nothing, so
//! it cannot delete the rows of the Session it conflicted with. Ids already
//! stored keep their bytes: restoration reads them as written, deletion plus
//! use as a fork source are unchanged, and deleting the stored row unblocks
//! the id it conflicted with.

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
            "EXPLAIN QUERY PLAN SELECT session_id FROM agent_sessions WHERE session_id = ?1 COLLATE NOCASE",
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

/// Legacy ids stored before the admission rule can share an output directory
/// with a newly admitted lowercase id under a supported filesystem's identity
/// rules even though they differ byte-for-byte and under ASCII case folding:
/// Unicode folding pairs (Kelvin sign, long s, ß/ẞ, ligatures), Unicode
/// default-ignorable code points (casefold volumes), and the Windows shapes —
/// backslash and alternate-stream separators, trailing dots and spaces,
/// Unicode mappings — plus the 8.3 short names, both plain and hashed. The
/// write entries must refuse such a candidate with InvalidInput before
/// anything is published, while the stored row keeps its bytes.
async fn assert_directory_shape_rejected(stored: &str, candidate: &str, why: &str) {
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
    seed_stored_session(&connection, stored);
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

    let created = sessions
        .create_owned_session(
            candidate,
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await;
    assert!(
        matches!(created, Err(AgentError::InvalidInput(_))),
        "{why}: creating {candidate:?} while {stored:?} is stored must be InvalidInput, got {created:?}"
    );
    let forked = sessions.fork_session("source", candidate, 1, &config).await;
    assert!(
        matches!(forked, Err(AgentError::InvalidInput(_))),
        "{why}: forking into {candidate:?} while {stored:?} is stored must be InvalidInput, got {forked:?}"
    );
    assert_eq!(
        stored_rows(&connection, candidate),
        0,
        "{why}: the refused candidate {candidate:?} writes nothing"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// One regression per output-directory-sharing shape (stored, candidate).
macro_rules! directory_shape {
    ($name:ident, $stored:expr, $candidate:expr, $why:expr) => {
        #[tokio::test]
        async fn $name() {
            assert_directory_shape_rejected($stored, $candidate, $why).await;
        }
    };
}

directory_shape!(
    kelvin_sign_folds_to_ascii_k,
    "ab\u{212A}",
    "abk",
    "APFS/HFS+/Linux-casefold fold the Kelvin sign U+212A to k"
);
directory_shape!(
    long_s_folds_to_ascii_s,
    "ab\u{17F}",
    "abs",
    "the long s U+017F folds to s under both volume rules"
);
directory_shape!(
    sharp_s_uppercases_to_ss,
    "ab\u{DF}",
    "abss",
    "ß uppercases to SS, so a casefold volume equates it with ss"
);
directory_shape!(
    uppercase_sharp_s_uppercases_to_ss,
    "ab\u{1E9E}",
    "abss",
    "ẞ uppercases to SS, so a casefold volume equates it with ss"
);
directory_shape!(
    ff_ligature_uppercases_to_ff,
    "ab\u{FB00}",
    "abff",
    "the ﬀ ligature uppercases to FF, so a casefold volume equates it with ff"
);
directory_shape!(
    default_ignorable_is_stripped_before_folding,
    "abc\u{AD}",
    "abc",
    "the soft hyphen U+00AD is a default-ignorable a casefold volume strips"
);
directory_shape!(
    windows_backslash_prefix_shares_the_first_segment,
    "abc\\def",
    "abc",
    "under Windows rules abc\\def names a path inside abc"
);
directory_shape!(
    windows_index_allocation_stream_shares_the_base,
    "abc::$INDEX_ALLOCATION",
    "abc",
    "under Windows rules abc::$INDEX_ALLOCATION is a stream of abc"
);
directory_shape!(
    windows_alternate_stream_shares_the_base,
    "abc:x",
    "abc",
    "under Windows rules abc:x is an alternate stream of abc"
);
directory_shape!(
    windows_trailing_dot_is_trimmed,
    "abc.",
    "abc",
    "under Windows rules a trailing dot is trimmed, so abc. is abc"
);
directory_shape!(
    windows_trailing_space_is_trimmed,
    "abc ",
    "abc",
    "under Windows rules a trailing space is trimmed, so abc␣ is abc"
);
directory_shape!(
    windows_dotted_capital_i_maps_to_i,
    "ab\u{130}",
    "abi",
    "under Windows rules the dotted capital İ maps onto i"
);
directory_shape!(
    windows_dotless_i_maps_to_i,
    "ab\u{131}",
    "abi",
    "under Windows rules the dotless ı maps onto i"
);
directory_shape!(
    windows_8_3_short_name_aliasing,
    "ab12cd~7",
    "ab12cdxyz",
    "an 8.3 volume may serve the long name ab12cdxyz as ab12cd~7"
);
directory_shape!(
    windows_8_3_hashed_short_name_aliasing,
    "ab12cd~7",
    "abwxyztuv",
    "an 8.3 volume may serve the long name abwxyztuv as the hashed ab12cd~7"
);

/// A trigger that stands in for another connection committing a Session of
/// `winner` the moment the copy writes its first `into` row for `fork` — the
/// timing a concurrent creation would have. Its rows ride the copy's own
/// transaction, so the interleaving is deterministic: `branches` makes the
/// commit land with the copy's first chunk, `events` with a chunk after it.
/// `INSERT OR IGNORE` keeps the copies of a later row from failing the
/// statement once the commit already happened.
fn commit_winner_when_copy_writes(connection: &Connection, into: &str, fork: &str, winner: &str) {
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER concurrent_creation AFTER INSERT ON {into}
             WHEN NEW.session_id = '{fork}'
             BEGIN
                 INSERT OR IGNORE INTO agent_sessions (session_id, config, created_at_ms, origin)
                     VALUES ('{winner}', '{{\"cwd\":\"/winner\"}}', 1, 'client');
                 INSERT OR IGNORE INTO session_catalog (session_id, updated_at_ms, preview, cwd)
                     VALUES ('{winner}', 1, 'winner-preview', '/winner');
                 INSERT OR IGNORE INTO session_metadata (session_id, revision, title, title_source)
                     VALUES ('{winner}', 1, 'winner-title', 'manual');
                 INSERT OR IGNORE INTO timeline_events (session_id, branch_id, sequence, last_sequence, kind, body, event_id)
                     VALUES ('{winner}', 'main', 99, 99, 'winner-kind', '{{\"winner\":true}}', 'winner-marker');
             END;"
        ))
        .unwrap();
}

/// The concurrent winner's own rows, asserted field by field: the copy's
/// cleanup must never reach a Session another connection committed.
fn assert_winner_intact(connection: &Connection, winner: &str) {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT config, origin FROM agent_sessions WHERE session_id=?1",
            [winner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap();
    let (config, origin) =
        row.unwrap_or_else(|| panic!("the winning Session {winner:?} lost its agent_sessions row"));
    assert_eq!(
        config, "{\"cwd\":\"/winner\"}",
        "the winning Session's config was overwritten"
    );
    assert_eq!(
        origin, "client",
        "the winning Session's origin was overwritten"
    );
    let catalog: Option<(i64, Option<String>, String)> = connection
        .query_row(
            "SELECT updated_at_ms, preview, cwd FROM session_catalog WHERE session_id=?1",
            [winner],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .unwrap();
    assert_eq!(
        catalog,
        Some((1, Some("winner-preview".to_owned()), "/winner".to_owned())),
        "the winning Session's catalog row must survive unchanged"
    );
    let metadata: Option<(i64, Option<String>)> = connection
        .query_row(
            "SELECT revision, title FROM session_metadata WHERE session_id=?1",
            [winner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap();
    assert_eq!(
        metadata,
        Some((1, Some("winner-title".to_owned()))),
        "the winning Session's metadata row must survive unchanged"
    );
    let marker: Option<(String, String)> = connection
        .query_row(
            "SELECT kind, body FROM timeline_events WHERE session_id=?1 AND branch_id='main' AND sequence=99",
            [winner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap();
    assert_eq!(
        marker,
        Some(("winner-kind".to_owned(), "{\"winner\":true}".to_owned())),
        "the winning Session's Timeline rows must survive the copy cleanup"
    );
}

/// #2204: another connection commits the fork's target byte-identically while
/// the copy's first chunk writes. The fork is refused and the winning Session
/// keeps every row it committed; the cleanup may only remove rows the fork
/// itself wrote.
#[tokio::test]
async fn a_concurrent_creation_during_the_first_copy_write_is_refused_and_the_winner_survives() {
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
    append(
        &store,
        key("source", "main"),
        vec![text(1, "first"), text(2, "second"), text(3, "third")],
    )
    .await
    .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    commit_winner_when_copy_writes(
        &connection,
        "timeline_branches",
        "fork-target",
        "fork-target",
    );
    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();

    let result = sessions
        .fork_session("source", "fork-target", 3, &config)
        .await;
    assert!(
        matches!(result, Err(AgentError::Storage(_))),
        "a byte-identical concurrent creation must keep the fork's Storage refusal, got {result:?}"
    );
    assert_winner_intact(&connection, "fork-target");

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// #2204: the concurrent creation commits after the copy's first chunks, so
/// only the final transaction's re-check can see it. The refusal must be the
/// re-check's, and the cleanup after it must still leave the winning Session
/// intact.
#[tokio::test]
async fn a_concurrent_creation_caught_by_the_final_recheck_keeps_the_winner_intact() {
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
    append(
        &store,
        key("source", "main"),
        vec![text(1, "first"), text(2, "second"), text(3, "third")],
    )
    .await
    .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    commit_winner_when_copy_writes(&connection, "timeline_events", "fork-target", "fork-target");
    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();

    let result = sessions
        .fork_session("source", "fork-target", 3, &config)
        .await;
    assert!(
        matches!(result, Err(AgentError::Storage(_))),
        "a byte-identical concurrent creation must keep the fork's Storage refusal, got {result:?}"
    );
    assert_winner_intact(&connection, "fork-target");

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// The final transaction's re-check tells a case-only variant apart from a
/// byte-identical duplicate: the concurrent creation commits `Fork-Target`
/// while the copy targets `fork-target`, so the refusal is InvalidInput and
/// the variant's rows stay untouched.
#[tokio::test]
async fn the_final_recheck_rejects_a_case_variant_with_invalid_input() {
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
    append(
        &store,
        key("source", "main"),
        vec![text(1, "first"), text(2, "second"), text(3, "third")],
    )
    .await
    .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    commit_winner_when_copy_writes(&connection, "timeline_events", "fork-target", "Fork-Target");
    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();

    let result = sessions
        .fork_session("source", "fork-target", 3, &config)
        .await;
    assert!(
        matches!(result, Err(AgentError::InvalidInput(_))),
        "a concurrent creation differing only by case must be InvalidInput, got {result:?}"
    );
    assert_winner_intact(&connection, "Fork-Target");

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// The conflict lookup is per stored row, not a frozen key set: once the
/// legacy row is deleted, the id it blocked is admitted again, on both the
/// creation and the fork entry.
#[tokio::test]
async fn deleting_the_legacy_row_unblocks_the_shape_it_conflicted_with() {
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
    seed_stored_session(&connection, "ab\u{212A}");

    let blocked = sessions
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
        matches!(blocked, Err(AgentError::InvalidInput(_))),
        "the case variant is refused while the legacy row is stored, got {blocked:?}"
    );
    let shaped = sessions
        .create_owned_session(
            "abk",
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await;
    assert!(
        matches!(shaped, Err(AgentError::InvalidInput(_))),
        "the folding variant is refused while the legacy row is stored, got {shaped:?}"
    );

    assert!(sessions.delete_session("Legacy-1").await.unwrap());
    connection
        .execute(
            "DELETE FROM agent_sessions WHERE session_id='ab\u{212A}'",
            [],
        )
        .unwrap();

    sessions
        .create_owned_session(
            "legacy-1",
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await
        .unwrap();
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
    sessions
        .fork_session("source", "abk", 1, &config)
        .await
        .unwrap();

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// #2204: the directory-shape lookup the write entries run must resolve
/// through the covering partial derived index — verbatim as the index defines
/// it — instead of scanning `agent_sessions`.
#[tokio::test]
async fn directory_shape_check_uses_the_derived_index() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let connection = Connection::open(&database.0).unwrap();
    let plan: Vec<String> = connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT session_id FROM agent_sessions WHERE session_id GLOB '*[^0-9A-Za-z_-]*'",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| { step.contains("agent_sessions_session_id_non_ascii_word") }),
        "the directory-shape check must use the covering index, plan: {plan:?}"
    );

    drop(connection);
    db.close().await.unwrap();
}

/// A write ledger for one fork target: every row the copy inserts or deletes
/// under the fork's id in the tables the copy touches is recorded, including
/// the cleanup's deletions. A fork refused before its first write must leave
/// the ledger empty — zero writes, zero cleanup.
fn ledger_fork_writes(connection: &Connection, fork: &str) {
    connection
        .execute_batch(&format!(
            "CREATE TABLE copy_ledger (entry TEXT);
             CREATE TRIGGER ledger_branches_insert AFTER INSERT ON timeline_branches
             WHEN NEW.session_id = '{fork}'
             BEGIN INSERT INTO copy_ledger VALUES ('timeline_branches'); END;
             CREATE TRIGGER ledger_events_insert AFTER INSERT ON timeline_events
             WHEN NEW.session_id = '{fork}'
             BEGIN INSERT INTO copy_ledger VALUES ('timeline_events'); END;
             CREATE TRIGGER ledger_content_insert AFTER INSERT ON timeline_content
             WHEN NEW.session_id = '{fork}'
             BEGIN INSERT INTO copy_ledger VALUES ('timeline_content'); END;
             CREATE TRIGGER ledger_sessions_insert AFTER INSERT ON agent_sessions
             WHEN NEW.session_id = '{fork}'
             BEGIN INSERT INTO copy_ledger VALUES ('agent_sessions'); END;
             CREATE TRIGGER ledger_branches_delete AFTER DELETE ON timeline_branches
             WHEN OLD.session_id = '{fork}'
             BEGIN INSERT INTO copy_ledger VALUES ('delete-timeline_branches'); END;
             CREATE TRIGGER ledger_events_delete AFTER DELETE ON timeline_events
             WHEN OLD.session_id = '{fork}'
             BEGIN INSERT INTO copy_ledger VALUES ('delete-timeline_events'); END;
             CREATE TRIGGER ledger_content_delete AFTER DELETE ON timeline_content
             WHEN OLD.session_id = '{fork}'
             BEGIN INSERT INTO copy_ledger VALUES ('delete-timeline_content'); END;
             CREATE TRIGGER ledger_sessions_delete AFTER DELETE ON agent_sessions
             WHEN OLD.session_id = '{fork}'
             BEGIN INSERT INTO copy_ledger VALUES ('delete-agent_sessions'); END;"
        ))
        .unwrap();
}

fn ledger_entries(connection: &Connection) -> Vec<String> {
    connection
        .prepare("SELECT entry FROM copy_ledger")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

/// A fork refused by a conflict that precedes the copy writes nothing and
/// runs no cleanup: the ledger proves not one row of the fork's id was ever
/// inserted or deleted, and the stored Session it conflicted with is
/// untouched. Shared by the case-collision and the directory-shape refusal.
async fn assert_refused_fork_never_writes(seed: &str, fork: &str, error: &str) {
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
    append(
        &store,
        key("source", "main"),
        vec![text(1, "first"), text(2, "second"), text(3, "third")],
    )
    .await
    .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    seed_stored_session(&connection, seed);
    ledger_fork_writes(&connection, fork);
    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();

    let result = sessions.fork_session("source", fork, 3, &config).await;
    match error {
        "storage" => assert!(
            matches!(result, Err(AgentError::Storage(_))),
            "the refused fork must keep its Storage error, got {result:?}"
        ),
        _ => assert!(
            matches!(result, Err(AgentError::InvalidInput(_))),
            "the refused fork must be InvalidInput, got {result:?}"
        ),
    }
    assert!(
        ledger_entries(&connection).is_empty(),
        "a fork refused before its first write may not write or delete a single row, ledger: {:?}",
        ledger_entries(&connection)
    );
    assert_eq!(
        stored_rows(&connection, seed),
        1,
        "the stored Session {seed:?} the fork conflicted with is untouched"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// #2204 (decision Q2 c (1)): a stored Session differing from the fork target
/// only by case refuses the fork inside the first chunk transaction, before
/// its first write — zero inserts, zero cleanup.
#[tokio::test]
async fn a_fork_conflicting_by_case_is_refused_with_zero_writes_and_zero_cleanup() {
    assert_refused_fork_never_writes("Fork-Case", "fork-case", "invalid").await;
}

/// #2204: the same zero-write guarantee covers the directory-shape check in
/// the first chunk transaction — the conflict the NOCASE lookup cannot see.
#[tokio::test]
async fn a_fork_conflicting_by_directory_shape_is_refused_with_zero_writes_and_zero_cleanup() {
    assert_refused_fork_never_writes("fork-shape\u{212A}", "fork-shapek", "invalid").await;
}

directory_shape!(
    windows_8_3_two_digit_tail_aliasing,
    "abcde~12",
    "abcdefghij",
    "an 8.3 volume may serve the long name abcdefghij as abcde~12 once the stem's collisions accumulate"
);

directory_shape!(
    mongolian_free_variation_selector_four_is_stripped_before_folding,
    "abc\u{180F}",
    "abc",
    "the Mongolian free variation selector four U+180F is a default-ignorable a casefold volume strips"
);

/// A stored id whose 8.3 stem is empty — the volume never generates such a
/// short name — must not collide with any long candidate, including the
/// ordinary UUID-shaped creation ids.
#[tokio::test]
async fn an_empty_8_3_stem_blocks_no_long_candidate() {
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
    seed_stored_session(&connection, "~1");

    sessions
        .create_owned_session(
            "abcdef123456",
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await
        .unwrap_or_else(|error| {
            panic!("a long candidate must not collide with an empty 8.3 stem: {error:?}")
        });
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
    sessions
        .fork_session("source", "0f83c2da-4b18-4c61-9d1e-2a7b3c4d5e6f", 1, &config)
        .await
        .unwrap_or_else(|error| {
            panic!("a UUID-shaped fork target must not collide with an empty 8.3 stem: {error:?}")
        });
    assert_eq!(
        stored_rows(&connection, "~1"),
        1,
        "the stored row is untouched"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// A store predating #2204 can hold a byte-identical duplicate and a
/// conflicting variant of the candidate at once. The duplicate's error takes
/// precedence on both write entries: it is the existing Session's protection,
/// while the variant's InvalidInput is only the admission refusal.
async fn assert_byte_identical_precedence(variant: &str, of: &str) {
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
    seed_stored_session(&connection, "abc");
    seed_stored_session(&connection, variant);
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

    let created = sessions
        .create_owned_session(
            "abc",
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await;
    assert!(
        matches!(created, Err(AgentError::Storage(_))),
        "creating abc while abc and {variant} ({of}) are stored must keep the duplicate's Storage error, got {created:?}"
    );
    let forked = sessions.fork_session("source", "abc", 1, &config).await;
    assert!(
        matches!(forked, Err(AgentError::Storage(_))),
        "forking into abc while abc and {variant} ({of}) are stored must keep the duplicate's Storage error, got {forked:?}"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM agent_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3,
        "abc, {variant} and the fork source stay stored, nothing else is written"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// #2204 (decision Q2 c (2)): a legacy store holding `abc` beside its
/// uppercase variant reports the byte-identical duplicate as Storage, not the
/// variant's InvalidInput.
#[tokio::test]
async fn a_byte_identical_duplicate_beats_a_case_variant_on_both_write_entries() {
    assert_byte_identical_precedence("ABC", "a case variant").await;
}

/// #2204 (decision Q2 c (2)): the precedence also covers a variant that
/// collides only through the directory-shape rules, so creation must not
/// report InvalidInput for a duplicate the fork entry already reports as
/// Storage.
#[tokio::test]
async fn a_byte_identical_duplicate_beats_a_directory_shape_variant_on_both_write_entries() {
    assert_byte_identical_precedence("abc.", "a directory-shape variant").await;
}

/// #2204: a fork refused inside the first chunk transaction — before its copy
/// committed anything — runs no cleanup at all. Rows already present under
/// the fork's id, written around every validation the way an external writer
/// or a crashed earlier copy could leave them, must survive the refusal; the
/// ledger records neither an insert nor a delete.
#[tokio::test]
async fn a_fork_refused_before_its_first_write_deletes_no_preexisting_rows() {
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
    append(
        &store,
        key("source", "main"),
        vec![text(1, "first"), text(2, "second"), text(3, "third")],
    )
    .await
    .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    seed_stored_session(&connection, "Fork-Keep");
    // The row stands in for what an external writer or a crashed copy leaves
    // under an id no Session owns: it cannot satisfy the branch foreign key,
    // so it is written with the checks off, the way such rows exist.
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO timeline_branches (session_id, branch_id, info, sequence) \
             VALUES ('fork-keep', 'main', '{\"id\":\"main\",\"parent\":null,\"branch_point\":null,\"create_anchor\":null}', 0);
             PRAGMA foreign_keys = ON;",
        )
        .unwrap();
    ledger_fork_writes(&connection, "fork-keep");
    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();

    let result = sessions
        .fork_session("source", "fork-keep", 3, &config)
        .await;
    assert!(
        matches!(result, Err(AgentError::InvalidInput(_))),
        "the case-variant fork must be refused with InvalidInput, got {result:?}"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_branches WHERE session_id='fork-keep'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1,
        "the rows already present under the fork's id must survive a refusal that wrote nothing"
    );
    assert!(
        ledger_entries(&connection).is_empty(),
        "the refusal may not write or delete a single row, ledger: {:?}",
        ledger_entries(&connection)
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

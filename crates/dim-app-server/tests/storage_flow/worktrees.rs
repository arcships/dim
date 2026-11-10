use super::*;
use dim_app_server::storage::{SessionStore, WorktreeDirectory, WorktreeUsage};

#[tokio::test]
async fn worktree_usage_survives_reopen_but_is_not_copied_by_fork_or_owned_by_sessions() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let worktree = WorktreeDirectory {
        repository: directory.join("repo/.git"),
        root: directory.join("repo-wt"),
    };
    std::fs::create_dir_all(worktree.root.join("packages/a")).unwrap();
    std::fs::create_dir_all(worktree.root.join("packages/b")).unwrap();
    let source_file = worktree.root.join("untracked.txt");
    std::fs::write(&source_file, "user content").unwrap();
    let root = rutis::Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![directory])))
        .await
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    for session in ["source", "other"] {
        store
            .create_session(session, &SessionConfig::default())
            .await
            .unwrap();
    }
    for _ in 0..2 {
        sessions
            .associate_worktree("source", worktree.clone(), worktree.root.clone())
            .await
            .unwrap();
    }
    sessions
        .associate_worktree("source", worktree.clone(), worktree.root.join("packages/a"))
        .await
        .unwrap();
    sessions
        .associate_worktree("other", worktree.clone(), worktree.root.join("packages/b"))
        .await
        .unwrap();
    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();
    sessions
        .fork_session("source", "fork", 0, &config)
        .await
        .unwrap();
    let expected = vec![
        WorktreeUsage {
            session_id: "other".into(),
            subtree: worktree.root.join("packages/b"),
        },
        WorktreeUsage {
            session_id: "source".into(),
            subtree: worktree.root.join("packages/a"),
        },
    ];
    assert_eq!(
        sessions.worktree_directories().await.unwrap(),
        std::slice::from_ref(&worktree)
    );
    assert_eq!(
        sessions.worktree_usages(worktree.clone()).await.unwrap(),
        expected
    );
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    assert_eq!(
        sessions.worktree_usages(worktree.clone()).await.unwrap(),
        expected
    );
    assert!(sessions.delete_session("source").await.unwrap());
    assert_eq!(
        sessions.worktree_usages(worktree.clone()).await.unwrap(),
        expected[..1]
    );
    assert!(sessions.delete_session("other").await.unwrap());
    assert!(sessions.delete_session("fork").await.unwrap());
    assert!(
        sessions
            .worktree_usages(worktree.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(sessions.worktree_directories().await.unwrap(), [worktree]);
    assert_eq!(
        std::fs::read_to_string(source_file).unwrap(),
        "user content"
    );
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_worktree_association_changes_neither_directory_nor_usage() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let worktree = WorktreeDirectory {
        repository: directory.join("repo/.git"),
        root: directory.join("repo-wt"),
    };
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = SessionStore::new(db.clone(), rutis::Ctx::root().unwrap(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    assert!(matches!(
        sessions.associate_worktree("absent", worktree.clone(), worktree.root.clone()).await,
        Err(AgentError::Closed(id)) if id == "absent"
    ));
    store
        .create_session("source", &SessionConfig::default())
        .await
        .unwrap();
    for subtree in [
        directory.join("repo-wt-other"),
        worktree.root.join("../outside"),
    ] {
        assert!(
            sessions
                .associate_worktree("source", worktree.clone(), subtree)
                .await
                .is_err()
        );
    }
    let relative = WorktreeDirectory {
        repository: PathBuf::from("repo/.git"),
        root: worktree.root.clone(),
    };
    assert!(
        sessions
            .associate_worktree("source", relative, worktree.root.clone())
            .await
            .is_err()
    );
    #[cfg(unix)]
    {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let invalid = WorktreeDirectory {
            repository: directory.join(OsString::from_vec(vec![0xff])),
            root: worktree.root.clone(),
        };
        assert!(
            sessions
                .associate_worktree("source", invalid, worktree.root.clone())
                .await
                .is_err()
        );
    }
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_worktree_usage BEFORE INSERT ON session_worktrees \
         BEGIN SELECT RAISE(ABORT, 'association storage failure'); END;",
        )
        .unwrap();
    assert!(
        sessions
            .associate_worktree("source", worktree.clone(), worktree.root.clone())
            .await
            .is_err()
    );
    assert!(sessions.worktree_directories().await.unwrap().is_empty());
    assert!(
        sessions
            .worktree_usages(worktree.clone())
            .await
            .unwrap()
            .is_empty()
    );
    connection
        .execute_batch("DROP TRIGGER fail_worktree_usage")
        .unwrap();
    sessions
        .associate_worktree("source", worktree.clone(), worktree.root.clone())
        .await
        .unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_worktree_update BEFORE UPDATE ON session_worktrees \
         BEGIN SELECT RAISE(ABORT, 'association update failure'); END;",
        )
        .unwrap();
    assert!(
        sessions
            .associate_worktree("source", worktree.clone(), worktree.root.join("changed"))
            .await
            .is_err()
    );
    assert_eq!(
        sessions.worktree_usages(worktree.clone()).await.unwrap(),
        [WorktreeUsage {
            session_id: "source".into(),
            subtree: worktree.root,
        }]
    );
    connection
        .execute_batch("DROP TRIGGER fail_worktree_update")
        .unwrap();
    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn worktree_association_cannot_outlive_delete_or_creation_rollback() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let root = rutis::Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![directory.clone()])))
        .await
        .unwrap();
    let worktree = WorktreeDirectory {
        repository: directory.join("repo/.git"),
        root: directory.join("repo-wt"),
    };
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("source", &SessionConfig::default())
        .await
        .unwrap();
    let (associated, deleted) = tokio::join!(
        sessions.associate_worktree("source", worktree.clone(), worktree.root.clone()),
        sessions.delete_session("source"),
    );
    assert!(associated.is_ok() || matches!(associated, Err(AgentError::Closed(_))));
    assert!(deleted.unwrap());
    assert!(
        sessions
            .worktree_usages(worktree.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        sessions
            .associate_worktree("source", worktree.clone(), worktree.root.clone())
            .await,
        Err(AgentError::Closed(_))
    ));
    store
        .create_session("rollback", &SessionConfig::default())
        .await
        .unwrap();
    sessions
        .associate_worktree("rollback", worktree.clone(), worktree.root.clone())
        .await
        .unwrap();
    store.rollback_session("rollback").await.unwrap();
    assert!(
        sessions
            .worktree_usages(worktree.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(sessions.worktree_directories().await.unwrap(), [worktree]);
    let connection = Connection::open(&database.0).unwrap();
    assert!(
        connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_v32_store_upgrades_without_inventing_worktree_usage() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(32);
    statements.sort_by_key(|sql| sql.starts_with("CREATE INDEX"));
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (32)", [])
        .unwrap();
    insert_documented_records(
        &connection,
        &BUSINESS_TABLES
            .iter()
            .copied()
            .filter(|table| !V33_TABLES.contains(table))
            .collect::<Vec<_>>(),
    );
    let before: Vec<_> = BUSINESS_TABLES
        .iter()
        .filter(|table| !V33_TABLES.contains(table))
        .map(|table| rows(&connection, table))
        .collect();
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(32));
    let sessions = SessionStore::new(db.clone(), rutis::Ctx::root().unwrap(), 0);
    assert!(sessions.worktree_directories().await.unwrap().is_empty());
    let store = SqliteTimelineStore::new(db.clone());
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(STORE_FORMAT));
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    let after: Vec<_> = BUSINESS_TABLES
        .iter()
        .filter(|table| !V33_TABLES.contains(table))
        .map(|table| rows(&connection, table))
        .collect();
    assert_eq!(after, before);
    assert!(rows(&connection, "session_worktrees").is_empty());
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    db.close().await.unwrap();
    let incompatible = Database::new();
    let connection = Connection::open(&incompatible.0).unwrap();
    let mut statements = schema_of(31);
    statements.sort_by_key(|sql| sql.starts_with("CREATE INDEX"));
    for sql in statements {
        connection
            .execute_batch(&sql.replace("(version = 31)", "(version = 32)"))
            .unwrap();
    }
    for sql in V33_ADDITIONS {
        connection.execute_batch(sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (32)", [])
        .unwrap();
    drop(connection);
    let before = std::fs::read(&incompatible.0).unwrap();
    assert!(SqliteStorage::open_read_only(incompatible.0.clone()).is_err());
    assert!(SqliteStorage::open(incompatible.0.clone()).is_err());
    assert_eq!(std::fs::read(&incompatible.0).unwrap(), before);
}

#![cfg(unix)]
use dim_agent_capabilities::defaults;
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_agent_types::EventKind;
use dim_app_server::{ServerConfig, serve, storage::SqliteStorage};
use dim_client::{Client, ConnectionConfig, Server};
use dim_migration::{Migration, MigrationPlugin, Outcome, TaskStatus};
use rusqlite::{Connection, params};
use rutis::host::CtxHost;
use serde_json::{Value, json};
use std::path::Path;

fn fixture(home: &Path) -> Connection {
    std::fs::create_dir_all(home).unwrap();
    let db = Connection::open(home.join("dimcode.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE sessions(sessionId TEXT PRIMARY KEY,cwd TEXT,title TEXT,status TEXT,tags TEXT,createdAt TEXT,updatedAt TEXT,version INTEGER);
        CREATE TABLE session_states(sessionId TEXT PRIMARY KEY,selectedProviderId TEXT,selectedModelId TEXT,settings TEXT,permissionSettings TEXT,currentMode TEXT);
        CREATE TABLE messages(messageId TEXT PRIMARY KEY,sessionId TEXT,role TEXT,parts TEXT,attachments TEXT,metadata TEXT,orderKey TEXT,createdAt TEXT);
        CREATE TABLE compaction_states(sessionId TEXT PRIMARY KEY,cursor TEXT,compactionSegments TEXT);
        CREATE TABLE session_blobs(blobId TEXT PRIMARY KEY,sessionId TEXT,kind TEXT,ref TEXT,mimeType TEXT,sizeBytes INTEGER,checksum TEXT);
        CREATE TABLE blob_data(blobId TEXT PRIMARY KEY,data BLOB);").unwrap();
    db.execute_batch("CREATE TABLE background_tasks(taskId TEXT PRIMARY KEY,sessionId TEXT,outputPath TEXT,status TEXT,completion TEXT);
        CREATE TABLE workflow_runs(workflowRunId TEXT PRIMARY KEY,sessionId TEXT,record TEXT);
        CREATE TABLE queue_items(queueItemId TEXT PRIMARY KEY,sessionId TEXT,payload TEXT,status TEXT);
        CREATE TABLE permission_decisions(decisionId TEXT PRIMARY KEY,sessionId TEXT,decision TEXT,remember INTEGER);
        CREATE TABLE usage_ledger(ledgerId TEXT PRIMARY KEY,sessionId TEXT,usage TEXT);").unwrap();
    std::fs::create_dir_all(home.join("data/jobs")).unwrap();
    std::fs::write(
        home.join("data/jobs/old-output.txt"),
        b"historical output bytes",
    )
    .unwrap();
    db.execute("INSERT INTO background_tasks VALUES ('old-task','old','data/jobs/old-output.txt','running','{\"result\":\"old result\"}')",[]).unwrap();
    db.execute("INSERT INTO queue_items VALUES ('old-queue','old','{\"text\":\"never automatically submit this queued input\"}','pending')",[]).unwrap();
    db.execute(
        "INSERT INTO permission_decisions VALUES ('one-shot','old','allow',0)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO usage_ledger VALUES ('old-usage','old','{\"inputTokens\":123}')",
        [],
    )
    .unwrap();
    let workflow = home.join("data/workflows/sessions/old/old-workflow");
    std::fs::create_dir_all(&workflow).unwrap();
    std::fs::write(
        workflow.join("workflow.ts"),
        b"throw new Error('never execute imported Workflow code')",
    )
    .unwrap();
    std::fs::write(
        workflow.join("meta.json"),
        b"{\"workflowRunId\":\"old-workflow\"}",
    )
    .unwrap();
    db.execute("INSERT INTO workflow_runs VALUES ('old-workflow','old','{\"source\":{\"kind\":\"inline\"},\"status\":\"running\",\"result\":\"historical workflow result\"}')",[]).unwrap();
    for (id, status) in [
        ("old", "active"),
        ("archived", "archived"),
        ("broken", "active"),
    ] {
        db.execute(
            "INSERT INTO sessions VALUES (?1,?2,'Old title',?3,'[]','2026-01-01','2026-01-01',1)",
            params![id, home.canonicalize().unwrap().to_str().unwrap(), status],
        )
        .unwrap();
        db.execute(
            "INSERT INTO session_states VALUES (?1,'mock','deterministic','{}','{}','agent')",
            [id],
        )
        .unwrap();
    }
    db.execute(
        "UPDATE sessions SET tags='[\"not-yet-supported\"]' WHERE sessionId='old'",
        [],
    )
    .unwrap();
    db.execute("UPDATE session_states SET settings='{\"__privateUnsupported__\":true}', currentMode='goal' WHERE sessionId='old'", []).unwrap();
    for (id, session, role, text, order) in [
        ("first", "old", "user", "compressed original", "a"),
        ("tail", "old", "assistant", "effective suffix", "b"),
        ("bad", "broken", "unknown", "bad role", "a"),
    ] {
        db.execute(
            "INSERT INTO messages VALUES (?1,?2,?3,?4,'[]','{}',?5,'2026-01-01T00:00:00Z')",
            params![
                id,
                session,
                role,
                json!([{"type":"text","text":text}]).to_string(),
                order
            ],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO compaction_states VALUES ('old','first',?1)",
        [
            json!([{"endMessageId":"first","summary":"retained summary","rawItems":[]}])
                .to_string(),
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO session_blobs VALUES ('attachment','old','text','unused','text/plain',7,NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO blob_data VALUES ('attachment',?1)",
        [b"payload".as_slice()],
    )
    .unwrap();
    std::fs::write(
        home.join("config.json"),
        json!({"locale":"ja","tuiNotifyOnComplete":false,"themeId":"system","subagentModel":{"providerId":"mock","modelId":"deterministic","reasoningEffort":"high"}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join("desktop-config.json"),
        json!({"version":7,"settings":{"locale":"zh-CN"}}).to_string(),
    )
    .unwrap();
    db
}

fn snapshot(home: &Path) -> Vec<(String, Vec<u8>)> {
    [
        "dimcode.sqlite",
        "dimcode.sqlite-wal",
        "dimcode.sqlite-shm",
        "schedule-tui.sqlite",
        "schedule-tui.sqlite-wal",
        "schedule-tui.sqlite-shm",
        "config.json",
        "desktop-config.json",
        "auth.json",
        "mcp-oauth.json",
    ]
    .into_iter()
    .filter(|name| home.join(name).exists())
    .map(|name| (name.into(), std::fs::read(home.join(name)).unwrap()))
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_retains_unsafe_objects_and_cleans_interrupted_snapshots() {
    #[cfg(target_os = "linux")]
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let db = fixture(&source);
    db.execute(
        "UPDATE messages SET role='user' WHERE sessionId='broken'",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO session_blobs VALUES ('bad-ref','broken','text','../../outside','text/plain',1,NULL)", []).unwrap();
    std::fs::write(source.join("permissions.json"), json!({"version":1,"execCommandPermissions":{"rules":{"git * --force":"deny","git *":"allow"}}}).to_string()).unwrap();
    #[cfg(target_os = "linux")]
    let plugin_dir = source
        .join("data/plugins")
        .join(OsString::from_vec(vec![0xff]));
    #[cfg(target_os = "linux")]
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let target = directory.path().join("target");
    let stale = target.join("migration/snapshots/interrupted");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("legacy.sqlite"), b"private snapshot").unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target.clone(),
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let migration = root.get::<Migration>().unwrap();
    migration.inspect(source.clone(), true).await.unwrap();
    assert!(!stale.exists());
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("legacy.sqlite"), b"private snapshot").unwrap();
    let before = snapshot(&source);
    let report = migration.run(source.clone(), true, None).await.unwrap();
    assert_eq!(report.task.status, TaskStatus::Partial, "{report:?}");
    for (id, outcome) in [
        ("permissions.json", Outcome::Unsupported),
        ("broken", Outcome::Failed),
        ("old", Outcome::Imported),
    ] {
        assert!(
            report
                .objects
                .iter()
                .any(|item| item.id == id && item.outcome == outcome),
            "{report:?}"
        );
    }
    let native = Connection::open(target.join("rust/dimcode.sqlite")).unwrap();
    assert_eq!(native.query_row("SELECT count(*) FROM agent_sessions s LEFT JOIN session_catalog c USING(session_id) WHERE c.session_id IS NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(native.query_row("SELECT count(*) FROM session_catalog c JOIN timeline_events e ON e.session_id=c.session_id AND e.branch_id='main' AND e.last_sequence=(SELECT sequence FROM timeline_branches WHERE session_id=c.session_id AND branch_id='main') WHERE c.updated_at_ms!=json_extract(e.body,'$.record.event.create_at')", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(
        native
            .query_row(
                "SELECT count(*) FROM settings_documents WHERE source='user/permission'",
                [],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
        0
    );
    assert!(!stale.exists());
    assert_eq!(snapshot(&source), before);
    #[cfg(target_os = "linux")]
    {
        assert!(plugin_dir.is_dir());
        assert!(
            report
                .objects
                .iter()
                .any(|item| item.id == "data/plugins" && item.outcome == Outcome::Deferred),
            "{report:?}"
        );
    }
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_validates_dim_account_tokens_before_writing_the_account_file() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let _db = fixture(&source);
    for field in [
        "valid",
        "clientId",
        "issuer",
        "tokenEndpoint",
        "relayBaseUrl",
        "authorizationEndpoint",
        "deviceAuthorizationEndpoint",
        "deviceTokenEndpoint",
        "scope",
        "expires",
    ] {
        let mut tokens = json!({"type":"oauth","access":"private-access","refresh":"private-refresh","expires":4102444800000u64,"clientId":"test-client","issuer":"https://auth.example.test","tokenEndpoint":"https://auth.example.test/token","relayBaseUrl":"https://api.example.test"});
        match field {
            "valid" => {}
            "clientId" => {
                tokens.as_object_mut().unwrap().remove(field);
            }
            "scope" => tokens[field] = json!(1),
            "expires" => tokens[field] = json!(0),
            _ => tokens[field] = json!("file:///private/invalid"),
        }
        std::fs::write(
            source.join("auth.json"),
            json!({"nextApiOauth":tokens}).to_string(),
        )
        .unwrap();
        let before = snapshot(&source);
        let target = directory.path().join(field);
        std::fs::create_dir_all(&target).unwrap();
        let root = rutis::Ctx::root().unwrap();
        let plugin = root.plugin(MigrationPlugin::new(
            target.clone(),
            "rust".into(),
            vec!["system".into()],
        ));
        (&plugin).await.unwrap();
        let report = root
            .get::<Migration>()
            .unwrap()
            .run(source.clone(), true, None)
            .await
            .unwrap();
        let account = report
            .objects
            .iter()
            .find(|item| item.kind == "account" && item.id == "dimcode-api-oauth")
            .unwrap();
        if field == "valid" {
            assert_eq!(account.outcome, Outcome::Imported, "{report:?}");
            let auth: Value =
                serde_json::from_slice(&std::fs::read(target.join("auth.json")).unwrap()).unwrap();
            assert_eq!(auth["nextApiOauth"], tokens);
        } else {
            assert_eq!(account.outcome, Outcome::Unsupported, "{field}: {report:?}");
            assert!(!target.join("auth.json").exists());
        }
        assert_eq!(snapshot(&source), before);
        plugin.dispose().await.unwrap();
        root.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrated_dim_account_states_are_readable_by_the_account_owner() {
    use dim_app_server::managed_provider::ManagedProviders;
    use sha2::{Digest, Sha256};

    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        directory.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let source = directory.path().join("source");
    let _db = fixture(&source);
    let tokens = json!({"type":"oauth","access":"private-access","refresh":"private-refresh","expires":4102444800000u64,"clientId":"test-client","issuer":"https://auth.example.test","tokenEndpoint":"https://auth.example.test/token","relayBaseUrl":"https://api.example.test"});
    let digest = format!("{:x}", Sha256::digest(serde_jcs::to_vec(&tokens).unwrap()));
    for case in [
        "authenticated",
        "signed_out",
        "integer_numbers",
        "nil_epoch",
        "missing_generation",
        "zero_generation",
        "fractional_generation",
        "unsafe_generation",
        "zero_revision",
        "unsafe_revision",
        "simple_epoch",
        "braced_epoch",
        "urn_epoch",
        "unknown_state",
        "missing_state",
        "signed_out_tokens",
        "signed_out_identity",
        "signed_out_digest",
        "authenticated_without_tokens",
        "missing_identity",
        "blank_subject",
        "invalid_issuer",
        "different_issuer",
        "invalid_email",
        "invalid_name",
        "wrong_digest",
    ] {
        let mut auth = json!({"nextApiOauth":tokens,"dimAccountState":{
            "schemaVersion":1,"credentialEpoch":"0b6f9a52-7e4b-4a59-9d0c-9a0c1c2f7e11",
            "generation":1,"credentialRevision":1,"state":"authenticated",
            "identity":{"issuer":"https://auth.example.test","subject":"legacy-subject"},"tokenDigest":digest
        }});
        if matches!(
            case,
            "signed_out" | "signed_out_tokens" | "signed_out_identity" | "signed_out_digest"
        ) {
            auth.as_object_mut().unwrap().remove("nextApiOauth");
            let state = auth["dimAccountState"].as_object_mut().unwrap();
            state.insert("state".into(), json!("signed_out"));
            state.remove("identity");
            state.remove("tokenDigest");
        }
        match case {
            "authenticated" | "signed_out" => {}
            "integer_numbers" => {
                auth["dimAccountState"]["generation"] = json!(1.0);
                auth["dimAccountState"]["credentialRevision"] = json!(1.0);
            }
            "nil_epoch" => {
                auth["dimAccountState"]["credentialEpoch"] = json!(uuid::Uuid::nil().to_string())
            }
            "missing_generation" => {
                auth["dimAccountState"]
                    .as_object_mut()
                    .unwrap()
                    .remove("generation");
            }
            "zero_generation" => auth["dimAccountState"]["generation"] = json!(0),
            "fractional_generation" => auth["dimAccountState"]["generation"] = json!(1.5),
            "unsafe_generation" => {
                auth["dimAccountState"]["generation"] =
                    json!(dim_agent_types::JS_MAX_SAFE_INTEGER + 1)
            }
            "zero_revision" => auth["dimAccountState"]["credentialRevision"] = json!(0),
            "unsafe_revision" => {
                auth["dimAccountState"]["credentialRevision"] =
                    json!(dim_agent_types::JS_MAX_SAFE_INTEGER + 1)
            }
            "simple_epoch" => {
                auth["dimAccountState"]["credentialEpoch"] =
                    json!("0b6f9a527e4b4a599d0c9a0c1c2f7e11")
            }
            "braced_epoch" => {
                auth["dimAccountState"]["credentialEpoch"] =
                    json!("{0b6f9a52-7e4b-4a59-9d0c-9a0c1c2f7e11}")
            }
            "urn_epoch" => {
                auth["dimAccountState"]["credentialEpoch"] =
                    json!("urn:uuid:0b6f9a52-7e4b-4a59-9d0c-9a0c1c2f7e11")
            }
            "unknown_state" => auth["dimAccountState"]["state"] = json!("other"),
            "missing_state" => {
                auth["dimAccountState"]
                    .as_object_mut()
                    .unwrap()
                    .remove("state");
            }
            "signed_out_tokens" => auth["nextApiOauth"] = tokens.clone(),
            "signed_out_identity" => auth["dimAccountState"]["identity"] = Value::Null,
            "signed_out_digest" => auth["dimAccountState"]["tokenDigest"] = Value::Null,
            "authenticated_without_tokens" => {
                auth.as_object_mut().unwrap().remove("nextApiOauth");
            }
            "missing_identity" => {
                auth["dimAccountState"]
                    .as_object_mut()
                    .unwrap()
                    .remove("identity");
            }
            "blank_subject" => auth["dimAccountState"]["identity"]["subject"] = json!(" "),
            "invalid_issuer" => {
                auth["dimAccountState"]["identity"]["issuer"] = json!("file:///invalid")
            }
            "different_issuer" => {
                auth["dimAccountState"]["identity"]["issuer"] =
                    json!("https://different.example.test")
            }
            "invalid_email" => auth["dimAccountState"]["identity"]["email"] = Value::Null,
            "invalid_name" => auth["dimAccountState"]["identity"]["name"] = json!(1),
            "wrong_digest" => auth["dimAccountState"]["tokenDigest"] = json!("0".repeat(64)),
            _ => unreachable!(),
        }
        std::fs::write(source.join("auth.json"), auth.to_string()).unwrap();
        let before = snapshot(&source);
        let target = directory.path().join(case);
        std::fs::create_dir_all(&target).unwrap();
        let original = b"{\"unrelated\":{\"keep\":true}}";
        std::fs::write(target.join("auth.json"), original).unwrap();
        let root = rutis::Ctx::root().unwrap();
        let plugin = root.plugin(MigrationPlugin::new(
            target.clone(),
            "rust".into(),
            vec!["system".into()],
        ));
        (&plugin).await.unwrap();
        let report = root
            .get::<Migration>()
            .unwrap()
            .run(source.clone(), true, None)
            .await
            .unwrap();
        let result = report
            .objects
            .iter()
            .find(|item| item.kind == "account" && item.id == "dimcode-api-oauth")
            .unwrap();
        let valid = matches!(
            case,
            "authenticated" | "signed_out" | "integer_numbers" | "nil_epoch"
        );
        assert_eq!(
            result.outcome,
            if valid {
                Outcome::Imported
            } else {
                Outcome::Unsupported
            },
            "{case}: {report:?}"
        );
        if !valid {
            assert_eq!(
                std::fs::read(target.join("auth.json")).unwrap(),
                original,
                "{case}"
            );
        }
        assert_eq!(snapshot(&source), before);
        plugin.dispose().await.unwrap();
        root.shutdown().await.unwrap();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install(&mut builder);
        defaults::mock_hosts(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                startup_timeout: std::time::Duration::from_secs(60),
                database_path: Some(target.join("rust/dimcode.sqlite")),
                product_home: Some(target),
                unix_path: Some(directory.path().join("native.sock")),
                managed_provider: Some(dim_oauth::row(dim_oauth::ClientIds {
                    global: "test-global".into(),
                    cn: "test-cn".into(),
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let account = server
            .service::<ManagedProviders>()
            .unwrap()
            .status(
                "dimcode-api-oauth",
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("{case}: native account reader rejected the target: {error}")
            });
        assert_eq!(
            account.account.is_some(),
            valid && case != "signed_out",
            "{case}"
        );
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_keeps_large_images_as_files_without_rejecting_session_history() {
    use base64::Engine;
    use dim_agent_types::{ContextCompactionResult, MessageContent, MessagePart};

    for (small_size, large_size) in [
        (1024 * 1024, 7 * 1024 * 1024),
        (3 * 1024 * 1024, 3 * 1024 * 1024),
    ] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            directory.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let source = directory.path().join("source");
        let db = fixture(&source);
        let small = vec![b'S'; small_size];
        let large = vec![b'L'; large_size];
        for (id, order, bytes) in [("small", "c", &small), ("large", "d", &large)] {
            db.execute(
                "INSERT INTO session_blobs VALUES (?1,'old','binary','unused','image/png',?2,NULL)",
                params![id, i64::try_from(bytes.len()).unwrap()],
            )
            .unwrap();
            db.execute("INSERT INTO blob_data VALUES (?1,?2)", params![id, bytes])
                .unwrap();
            db.execute("INSERT INTO messages VALUES (?1,'old','user','[{\"type\":\"text\",\"text\":\"image attachment\"}]',?2,'{}',?3,'2026-01-01T00:00:00Z')", params![
                id,
                json!([{"attachmentId":id,"kind":"blob","data":{"blobId":id,"mediaType":"image/png"}}]).to_string(),
                order,
            ]).unwrap();
        }
        let before = snapshot(&source);
        let target = directory.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        let root = rutis::Ctx::root().unwrap();
        let plugin = root.plugin(MigrationPlugin::new(
            target.clone(),
            "rust".into(),
            vec!["system".into()],
        ));
        (&plugin).await.unwrap();
        let migration = root.get::<Migration>().unwrap();
        let report = migration.run(source.clone(), true, None).await.unwrap();
        {
            let progress = migration.progress();
            let progress = progress.borrow();
            let progress = progress.as_ref().unwrap();
            assert_eq!(progress.task.id, report.task.id);
            assert_eq!(progress.recent.len(), report.objects.len().min(8));
        }
        let imported = report
            .objects
            .iter()
            .find(|r| r.kind == "session" && r.id == "old")
            .unwrap();
        assert_eq!(imported.outcome, Outcome::Imported, "{report:?}");
        assert!(
            imported
                .omitted
                .iter()
                .any(|field| field == "attachment.inlineImage.large")
        );
        assert!(
            !imported
                .omitted
                .iter()
                .any(|field| field == "attachment.inlineImage.small")
        );
        let id = imported.target_id.clone().unwrap();
        plugin.dispose().await.unwrap();
        root.shutdown().await.unwrap();

        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install(&mut builder);
        defaults::mock_hosts(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                startup_timeout: std::time::Duration::from_secs(60),
                database_path: Some(target.join("rust/dimcode.sqlite")),
                product_home: Some(target),
                unix_path: Some(directory.path().join("native.sock")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let session = server.agent().unwrap().restore_session(id).await.unwrap();
        let history = session.history("main", None).await.unwrap();
        let replacement = history
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ContextCompaction(data) => match &data.outcome {
                    ContextCompactionResult::Completed { replacement, .. } => Some(replacement),
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        let parts: Vec<_> = replacement
            .iter()
            .filter_map(|message| match &message.content {
                MessageContent::Parts(parts) => Some(parts),
                _ => None,
            })
            .flatten()
            .collect();
        let images: Vec<_> = parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Image { source } => Some(source),
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 1);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(images[0].data.as_ref().unwrap())
                .unwrap(),
            small
        );
        let path = parts
            .iter()
            .find_map(|part| match part {
                MessagePart::Text { text, .. } => {
                    text.strip_prefix("Attached imported blob large: ")
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), large);
        assert!(history.iter().any(|event| matches!(&event.kind, EventKind::InputMessage(input) if input.user.as_ref().is_some_and(|user| user.content.iter().any(|part|matches!(part, dim_agent_types::ContentBlock::Text {text} if text.contains("Attached imported blob large: ")))))));
        assert!(
            history.iter().all(|event| !matches!(
                event.kind,
                EventKind::ModelCall(_) | EventKind::ToolCall(_)
            ))
        );
        assert_eq!(snapshot(&source), before);
        server.shutdown();
        server.join().await.unwrap();
        drop(db);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_import_reopens_with_unchanged_native_consumers_and_preserves_continuation() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        directory.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let source = directory.path().join("source");
    let db = fixture(&source);
    let target = directory.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let native_path = target.join("rust/dimcode.sqlite");
    let native = SqliteStorage::open(native_path.clone()).unwrap();
    native.close().await.unwrap();
    Connection::open(&native_path)
        .unwrap()
        .execute_batch(
            "CREATE INDEX timeline_events_by_end ON timeline_events(session_id, branch_id, last_sequence, sequence); DROP TABLE agent_store_format;
             CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 23));
             INSERT INTO agent_store_format VALUES (23);",
        )
        .unwrap();
    let (previous, version) = SqliteStorage::open_read_only(native_path).unwrap();
    assert_eq!(version, 23);
    previous.close().await.unwrap();
    std::fs::write(
        target.join("config.json"),
        r#"{"locale":"en","tuiToolDetailsExpanded":false}"#,
    )
    .unwrap();
    let before = snapshot(&source);
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target.clone(),
        Path::new("rust").to_owned(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let migration = root.get::<Migration>().unwrap();
    let report = migration.run(source.clone(), true, None).await.unwrap();
    assert_eq!(report.task.status, TaskStatus::Partial, "{report:?}");
    let imported = report
        .objects
        .iter()
        .find(|r| r.kind == "session" && r.id == "old")
        .unwrap();
    assert_eq!(imported.outcome, Outcome::Imported, "{report:?}");
    let id = imported.target_id.clone().unwrap();
    assert_eq!(snapshot(&source), before);
    assert_eq!(
        Connection::open(target.join("rust/dimcode.sqlite"))
            .unwrap()
            .query_row("SELECT version FROM agent_store_format", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        25
    );
    let retried = migration
        .run(source.clone(), true, Some(report.task.id.clone()))
        .await
        .unwrap();
    assert!(
        retried
            .objects
            .iter()
            .any(|r| r.id == "old" && r.outcome == Outcome::AlreadyImported),
        "{retried:?}"
    );
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: std::time::Duration::from_secs(60),
            database_path: Some(target.join("rust/dimcode.sqlite")),
            product_home: Some(target.clone()),
            unix_path: Some(directory.path().join("native.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    let connection = ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "migration-test",
    };
    let mut client = Client::connect(&connection).await.unwrap();
    let listed: Value = client.typed("session.list", json!({})).await.unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    let page: Value = client
        .typed("session.catalog", json!({"query":id,"withPreview":true}))
        .await
        .unwrap();
    assert_eq!(page["sessions"][0]["sessionId"], id);
    assert!(
        page["sessions"][0]["preview"]
            .as_str()
            .is_some_and(|preview| !preview.is_empty())
    );
    assert!(
        agent.sessions().is_empty(),
        "listing must not restore imported Sessions"
    );
    let session = agent
        .restore_session(id.clone())
        .await
        .expect("Native restore must accept imported data");
    assert_eq!(
        session.config().permission_policy.preset,
        dim_agent_types::PermissionPreset::WorkspaceWrite
    );
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    let history = session.history("main", None).await.unwrap();
    assert!(
        history
            .iter()
            .any(|e| matches!(&e.kind,EventKind::Text(t) if t.delta=="effective suffix"))
    );
    assert!(history.iter().all(|e| !matches!(
        e.kind,
        EventKind::ModelCall(_)
            | EventKind::ModelStart(_)
            | EventKind::ToolCall(_)
            | EventKind::ToolStart(_)
    )));
    client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","text":"continue"}),
        )
        .await
        .unwrap();
    client
        .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
        .await
        .unwrap();
    let history = session.history("main", None).await.unwrap();
    let request = history
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ModelCall(call) => Some(&call.request),
            _ => None,
        })
        .expect("Native continuation must reach the normal model host");
    let context = serde_json::to_string(&request.messages).unwrap();
    assert!(
        context.contains("Imported history")
            && context.contains("retained summary")
            && context.contains("effective suffix"),
        "{context}"
    );
    assert!(!context.contains("compressed original"), "{context}");
    assert!(
        request
            .messages
            .iter()
            .any(|m| m.role == dim_agent_types::Role::Assistant)
    );
    client
        .rpc("session.delete", json!({"sessionId":id}))
        .await
        .unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target,
        Path::new("rust").to_owned(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let report = root
        .get::<Migration>()
        .unwrap()
        .run(source.clone(), true, None)
        .await
        .unwrap();
    assert!(
        report
            .objects
            .iter()
            .any(|r| r.id == "old" && r.outcome == Outcome::Deleted),
        "{report:?}"
    );
    assert_eq!(snapshot(&source), before);
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    drop(db);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_defers_remote_compaction_without_sealing_a_future_incremental_import() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let db = fixture(&source);
    db.execute("UPDATE compaction_states SET compactionSegments=?1 WHERE sessionId='old'",[json!([{"endMessageId":"first","summary":"[remote-compacted]","rawItems":[{"type":"compaction","encrypted_content":"opaque"}]}]).to_string()]).unwrap();
    let target = directory.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target.clone(),
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let migration = root.get::<Migration>().unwrap();
    let before = snapshot(&source);
    let report = migration.run(source.clone(), true, None).await.unwrap();
    let remote = report
        .objects
        .iter()
        .find(|o| o.id == "old" && o.kind == "session")
        .unwrap();
    assert_eq!(remote.outcome, Outcome::Deferred, "{report:?}");
    assert!(remote.target_id.is_none());
    assert_eq!(snapshot(&source), before);
    let native = Connection::open(target.join("rust/dimcode.sqlite")).unwrap();
    assert_eq!(
        native
            .query_row("SELECT count(*) FROM agent_sessions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(native);
    db.execute(
        "UPDATE session_states SET settings=?1 WHERE sessionId='old'",
        [json!({"__revertState__":{"anchorOrderKey":"b"}}).to_string()],
    )
    .unwrap();
    let report = migration
        .run(source.clone(), true, Some(report.task.id))
        .await
        .unwrap();
    assert!(
        report
            .objects
            .iter()
            .any(|item| item.id == "old" && item.outcome == Outcome::Deferred),
        "{report:?}"
    );
    assert_eq!(
        Connection::open(target.join("rust/dimcode.sqlite"))
            .unwrap()
            .query_row("SELECT count(*) FROM agent_sessions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute(
        "UPDATE compaction_states SET compactionSegments=?1 WHERE sessionId='old'",
        [
            json!([{"endMessageId":"first","summary":"compatible summary","rawItems":[]}])
                .to_string(),
        ],
    )
    .unwrap();
    let report = migration
        .run(source, true, Some(report.task.id))
        .await
        .unwrap();
    assert!(
        report
            .objects
            .iter()
            .any(|o| o.id == "old" && o.outcome == Outcome::Imported),
        "{report:?}"
    );
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_writes_supported_provider_settings_accounts_and_schedule_as_normal_native_data()
{
    use sha2::{Digest, Sha256};
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        directory.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let source = directory.path().join("source");
    let db = fixture(&source);
    db.execute_batch("CREATE TABLE providers (providerId TEXT PRIMARY KEY, source TEXT, displayName TEXT, driverKind TEXT, defaultBaseUrl TEXT, baseUrl TEXT, auth TEXT, credential TEXT, accountUsageSettings TEXT, headers TEXT, proxy TEXT, timeoutMs INTEGER, retryConfig TEXT, models TEXT, modelConfigOverrides TEXT, capabilityDefaults TEXT, activeModelId TEXT, modelsSource TEXT, catalogVersion TEXT, modelsUpdatedAt TEXT, metadata TEXT, enabled INTEGER, createdAt TEXT, updatedAt TEXT, version INTEGER); CREATE TABLE provider_selections (selectionId TEXT PRIMARY KEY, scope TEXT, cwd TEXT, providerId TEXT, modelId TEXT, createdAt TEXT, updatedAt TEXT, version INTEGER);").unwrap();
    let models = json!([
        {"modelId":"legacy-model","displayName":"Legacy model","status":"active","capabilities":dim_agent_types::ModelCapabilities::default()},
        {"modelId":"disabled-model","displayName":"Disabled model","status":"active","capabilities":dim_agent_types::ModelCapabilities::default()},
        {"modelId":"unavailable-model","displayName":"Unavailable model","status":"unavailable","capabilities":dim_agent_types::ModelCapabilities::default()}
    ]);
    db.execute("INSERT INTO providers (providerId,source,displayName,driverKind,baseUrl,auth,credential,accountUsageSettings,models,modelsSource,metadata,enabled,version) VALUES ('legacy-provider','custom','Legacy','openai-responses','https://model.example/v1',?1,?2,?3,?4,'remote',?5,1,1)",params![json!({"type":"bearer"}).to_string(),json!({"type":"bearer","token":"private-provider-secret"}).to_string(),json!({"unsupported":"private-unused-secret"}).to_string(),models.to_string(),json!({"disabledModelIds":["disabled-model"]}).to_string()]).unwrap();
    db.execute("INSERT INTO provider_selections (selectionId,scope,providerId,modelId,version) VALUES ('chosen','global','legacy-provider','legacy-model',1)",[]).unwrap();
    let router_models = json!([
        {"modelId":"image-route","displayName":"Image route","status":"active","capabilities":dim_agent_types::ModelCapabilities::default(),"metadata":{"raw":{"endpointType":"image-generation"}}},
        {"modelId":"ambiguous-route","displayName":"Ambiguous route","status":"active","capabilities":dim_agent_types::ModelCapabilities::default(),"metadata":{"raw":{"selectable_endpoint_types":["anthropic","gemini"]}}},
        {"modelId":"claude-route","displayName":"Chat route","status":"active","capabilities":dim_agent_types::ModelCapabilities::default(),"metadata":{"raw":{"selectableEndpointTypes":["anthropic","gemini"]}}}
    ]);
    db.execute("INSERT INTO providers (providerId,source,displayName,driverKind,baseUrl,auth,credential,models,modelsSource,metadata,enabled,version) SELECT 'router',source,displayName,'new-api-router',baseUrl,auth,credential,?1,modelsSource,'{}',enabled,version FROM providers WHERE providerId='legacy-provider'", [router_models.to_string()]).unwrap();
    for (id, provider, model) in [
        ("missing-provider", "openai-codex", "legacy-model"),
        ("missing-model", "legacy-provider", "missing"),
        ("disabled-model", "legacy-provider", "disabled-model"),
        ("unavailable-model", "legacy-provider", "unavailable-model"),
        ("image-route", "router", "image-route"),
        ("ambiguous-route", "router", "ambiguous-route"),
        ("router-valid", "router", "claude-route"),
    ] {
        db.execute("INSERT INTO provider_selections (selectionId,scope,cwd,providerId,modelId,version) VALUES (?1,'workspace',?2,?3,?4,1)",params![id,source.join(id).to_str().unwrap(),provider,model]).unwrap();
    }
    let tokens = json!({"type":"oauth","refresh":"private-refresh","access":"private-access","expires":4102444800000u64,"issuer":"https://auth.openai.com","tokenEndpoint":"https://auth.openai.com/oauth/token","clientId":"app_EMoamEEZ73f0CkXaXp7hrann","scope":"openid profile email offline_access api.connectors.read api.connectors.invoke","relayBaseUrl":"https://chatgpt.com/backend-api/codex"});
    let digest = format!("{:x}", Sha256::digest(serde_jcs::to_vec(&tokens).unwrap()));
    std::fs::write(source.join("auth.json"),json!({"openaiCodex":tokens,"openaiCodexAccountState":{"schemaVersion":1,"generation":"0b6f9a52-7e4b-4a59-9d0c-9a0c1c2f7e11","credentialRevision":1,"tokenDigest":digest}}).to_string()).unwrap();
    std::fs::write(source.join("mcp.json"),json!({"mcpServers":{"docs":{"type":"http","url":"https://mcp.example.test","enabled":false,"oauth":{"clientId":"legacy-client","scopes":"files:read"}},"bad":{"type":"http","url":"https://mcp.example.test/bad","enabled":false}}}).to_string()).unwrap();
    let mcp_grant = json!({"serverUrl":"https://mcp.example.test","tokens":{"accessToken":"private-mcp-token","refreshToken":"private-mcp-refresh","expiresAt":4102444800000u64,"scope":"files:read"},"clientInfo":{"clientId":"legacy-client","metadata":{"redirectUri":"http://127.0.0.1:1456/callback"}},"authorizationMetadata":{"issuer":"https://login.example.test","token_endpoint":"https://login.example.test/token"},"protectedResourceMetadata":{"resource":"https://mcp.example.test"}});
    let mut bad_grant = mcp_grant.clone();
    bad_grant["serverUrl"] = json!("https://mcp.example.test/bad");
    bad_grant["tokens"]["expiresAt"] = json!("private-invalid-expiry");
    std::fs::write(
        source.join("mcp-oauth.json"),
        json!({"version":1,"servers":{"docs":mcp_grant,"bad":bad_grant}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        source.join("skills.json"),
        json!({"disabledSkillIds":["user:legacy-skill"]}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(source.join("data/plugins/web")).unwrap();
    std::fs::write(
        source.join("data/plugins/web/state.json"),
        json!({"version":1,"enabled":true,"activeProvider":"dim-oauth","activeProviderExplicit":true,"providers":{}}).to_string(),
    )
    .unwrap();
    db.execute_batch("CREATE TABLE scheduled_jobs(jobId TEXT PRIMARY KEY,name TEXT,prompt TEXT,status TEXT,rrule TEXT,cwd TEXT,providerId TEXT,modelId TEXT,reasoningEffort TEXT,sessionMode TEXT,boundSessionId TEXT,nextRunAt TEXT,lastRunAt TEXT,createdAt TEXT);").unwrap();
    db.execute("INSERT INTO scheduled_jobs VALUES ('paused','Paused job','do work','paused','FREQ=DAILY;BYHOUR=9;BYMINUTE=15',?1,'mock','deterministic',NULL,'new',NULL,NULL,NULL,'2026-01-01T00:00:00Z')",[source.to_str().unwrap()]).unwrap();
    let before = snapshot(&source);
    let target = directory.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("config.json"),
        r#"{"locale":null,"tuiNotifyOnComplete":false}"#,
    )
    .unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target.clone(),
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let migration = root.get::<Migration>().unwrap();
    let report = migration.run(source.clone(), true, None).await.unwrap();
    let provider = report
        .objects
        .iter()
        .find(|r| r.kind == "provider" && r.id == "legacy-provider")
        .unwrap();
    assert_eq!(provider.outcome, Outcome::Imported, "{report:?}");
    assert!(provider.omitted.contains(&"accountUsageSettings".into()));
    for id in [
        "missing-provider",
        "missing-model",
        "disabled-model",
        "unavailable-model",
        "image-route",
        "ambiguous-route",
    ] {
        assert!(
            report.objects.iter().any(|item| item.kind == "selection"
                && item.id == id
                && item.outcome == Outcome::Unsupported),
            "{report:?}"
        );
    }
    assert!(
        report
            .objects
            .iter()
            .any(|r| r.id == "openai-codex" && r.outcome == Outcome::Imported),
        "{report:?}"
    );
    assert!(
        report
            .objects
            .iter()
            .any(|r| r.id == "legacy-desktop:paused" && r.outcome == Outcome::Imported),
        "{report:?}"
    );
    assert_eq!(snapshot(&source), before);
    let safe = serde_json::to_string(&report).unwrap();
    assert!(
        !safe.contains("private-provider-secret")
            && !safe.contains("private-refresh")
            && !safe.contains("private-unused-secret")
    );
    let native = Connection::open(target.join("rust/dimcode.sqlite")).unwrap();
    assert_eq!(
        native
            .query_row("SELECT count(*) FROM provider_selections", [], |row| row
                .get::<_, u32>(0))
            .unwrap(),
        2,
        "{report:?}"
    );
    let schedule: String = native
        .query_row(
            "SELECT document FROM settings_documents WHERE source='user/schedule'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let schedule: Value = serde_json::from_str(&schedule).unwrap();
    assert_eq!(schedule["jobs"][0]["enabled"], false);
    assert_eq!(schedule["jobs"][0]["schedule"]["cron"], "15 9 * * *");
    let web: String = native
        .query_row(
            "SELECT document FROM settings_documents WHERE source='user/web'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let web: Value = serde_json::from_str(&web).unwrap();
    assert_eq!(web["searchService"]["kind"], "dim-oauth");
    native
        .execute(
            "UPDATE settings_documents SET document=?1 WHERE source='user/schedule'",
            [json!({"jobs":[],"max_concurrent_runs":4}).to_string()],
        )
        .unwrap();
    let retried = migration
        .run(source.clone(), true, Some(report.task.id.clone()))
        .await
        .unwrap();
    assert!(
        retried
            .objects
            .iter()
            .any(|item| item.kind == "scheduled_job"
                && item.id == "legacy-desktop:paused"
                && item.outcome == Outcome::Deleted),
        "{retried:?}"
    );
    native
        .execute(
            "DELETE FROM settings_documents WHERE source='user/schedule'",
            [],
        )
        .unwrap();
    let cleared = migration
        .run(source.clone(), true, Some(retried.task.id))
        .await
        .unwrap();
    assert!(
        cleared
            .objects
            .iter()
            .any(|item| item.kind == "scheduled_job"
                && item.id == "legacy-desktop:paused"
                && item.outcome == Outcome::Deleted),
        "{cleared:?}"
    );
    let config: Value =
        serde_json::from_slice(&std::fs::read(target.join("config.json")).unwrap()).unwrap();
    assert!(config["locale"].is_null());
    assert_eq!(config["tuiNotifyOnComplete"], false);
    drop(native);
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: std::time::Duration::from_secs(60),
            database_path: Some(target.join("rust/dimcode.sqlite")),
            product_home: Some(target.clone()),
            unix_path: Some(directory.path().join("native.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let config = ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "migration-test",
    };
    let mut client = Client::connect(&config).await.unwrap();
    let listed: Value = client.typed("provider.list", json!({})).await.unwrap();
    assert!(listed.to_string().contains("legacy-provider"), "{listed}");
    let status: Value = client
        .typed("account.codex.status", json!({}))
        .await
        .unwrap();
    assert_eq!(status["authenticated"], true, "{status}");
    let mcp: Value = client.typed("mcp.catalog", json!({})).await.unwrap();
    let docs = mcp["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|server| server["id"] == "docs")
        .unwrap();
    assert_eq!(docs["auth"]["status"], "authorized", "{mcp}");
    assert_eq!(docs["enabled"], false);
    assert!(
        report
            .objects
            .iter()
            .any(|result| result.id == "mcp:bad" && result.outcome == Outcome::Unsupported)
    );
    assert!(!safe.contains("private-mcp-token") && !safe.contains("private-invalid-expiry"));
    let skills: Value = client.typed("skills.catalog", json!({})).await.unwrap();
    assert!(skills["entries"].is_array(), "{skills}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let native = Connection::open(target.join("rust/dimcode.sqlite")).unwrap();
    native
        .execute(
            "UPDATE providers SET record='{}' WHERE provider_id='legacy-provider'",
            [],
        )
        .unwrap();
    db.execute("INSERT INTO provider_selections (selectionId,scope,cwd,providerId,modelId,version) VALUES ('corrupt-target','workspace',?1,'legacy-provider','legacy-model',1)", [source.join("corrupt-target").to_str().unwrap()]).unwrap();
    let before = snapshot(&source);
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target.clone(),
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let report = root
        .get::<Migration>()
        .unwrap()
        .run(source.clone(), true, None)
        .await
        .unwrap();
    assert_eq!(report.task.status, TaskStatus::Partial, "{report:?}");
    assert!(
        report.objects.iter().any(|item| item.kind == "selection"
            && item.id == "corrupt-target"
            && item.outcome == Outcome::Unsupported),
        "{report:?}"
    );
    assert_eq!(
        native
            .query_row(
                "SELECT record FROM providers WHERE provider_id='legacy-provider'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "{}"
    );
    assert_eq!(snapshot(&source), before);
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_preserves_removed_client_settings_and_refuses_unsupported_target_formats() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let _db = fixture(&source);
    let target = directory.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target.clone(),
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let migration = root.get::<Migration>().unwrap();
    let first = migration.run(source.clone(), true, None).await.unwrap();
    let path = target.join("config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config.as_object_mut().unwrap().remove("locale");
    std::fs::write(&path, config.to_string()).unwrap();
    let retry = migration
        .run(source.clone(), true, Some(first.task.id))
        .await
        .unwrap();
    assert!(
        retry
            .objects
            .iter()
            .any(|r| r.id == "config.json.locale" && r.outcome == Outcome::Deleted),
        "{retry:?}"
    );
    let config: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(config.get("locale").is_none());
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    let preview = directory.path().join("preview");
    std::fs::create_dir_all(preview.join("rust")).unwrap();
    let native = Connection::open(preview.join("rust/dimcode.sqlite")).unwrap();
    native.execute_batch("CREATE TABLE agent_store_format(version INTEGER PRIMARY KEY CHECK(version=25));INSERT INTO agent_store_format VALUES(25);CREATE TABLE important(data TEXT);INSERT INTO important VALUES('preserve');").unwrap();
    drop(native);
    let before = std::fs::read(preview.join("rust/dimcode.sqlite")).unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        preview.clone(),
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let report = root
        .get::<Migration>()
        .unwrap()
        .run(source, true, None)
        .await
        .unwrap();
    assert_eq!(report.task.status, TaskStatus::Failed);
    assert_eq!(
        std::fs::read(preview.join("rust/dimcode.sqlite")).unwrap(),
        before
    );
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_cancellation_settles_before_unload_and_a_new_load_can_retry() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let _db = fixture(&source);
    let before = snapshot(&source);
    let target = directory.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target.clone(),
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let migration = root.get::<Migration>().unwrap();
    let mut progress = migration.progress();
    let work = {
        let migration = migration.clone();
        let source = source.clone();
        tokio::spawn(async move { migration.run(source, true, None).await.unwrap() })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), progress.changed())
        .await
        .unwrap()
        .unwrap();
    migration.cancel_current();
    plugin.dispose().await.unwrap();
    let cancelled = work.await.unwrap();
    assert_eq!(
        cancelled.task.status,
        TaskStatus::Cancelled,
        "{cancelled:?}"
    );
    assert_eq!(snapshot(&source), before);
    root.shutdown().await.unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target,
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let report = root
        .get::<Migration>()
        .unwrap()
        .run(source.clone(), true, Some(cancelled.task.id))
        .await
        .unwrap();
    assert!(
        report
            .objects
            .iter()
            .any(|r| r.id == "old"
                && matches!(r.outcome, Outcome::Imported | Outcome::AlreadyImported)),
        "{report:?}"
    );
    assert_eq!(snapshot(&source), before);
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migration_continuation_preserves_tool_pairs_revert_and_retry_boundaries() {
    for case in ["tools", "revert", "revert_before_summary", "retry"] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            directory.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let source = directory.path().join("source");
        let db = fixture(&source);
        match case {
            "tools" => {
                db.execute("UPDATE messages SET role='assistant',parts=?1 WHERE messageId='first'",[json!([{"type":"tool_use","id":"old-call","name":"echo","input":{"text":"never replay"}}]).to_string()]).unwrap();
                db.execute("UPDATE messages SET role='tool_result',parts=?1 WHERE messageId='tail'",[json!([{"type":"tool_result","tool_use_id":"old-call","content":"historical output","structuredContent":{"exitCode":0}}]).to_string()]).unwrap();
            }
            "revert" | "revert_before_summary" => {
                db.execute(
                    "UPDATE session_states SET settings=?1 WHERE sessionId='old'",
                    [json!({"__revertState__":{"anchorOrderKey":if case == "revert" {"b"} else {"a"}}}).to_string()],
                )
                .unwrap();
            }
            "retry" => {
                db.execute("UPDATE messages SET metadata=?1 WHERE messageId='tail'",[json!({"_dimTranscript":{"kind":"provider_retry","visibility":"visible","owner":"runtime"}}).to_string()]).unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(&source);
        let target = directory.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        let root = rutis::Ctx::root().unwrap();
        let plugin = root.plugin(MigrationPlugin::new(
            target.clone(),
            "rust".into(),
            vec!["system".into()],
        ));
        (&plugin).await.unwrap();
        let report = root
            .get::<Migration>()
            .unwrap()
            .run(source.clone(), true, None)
            .await
            .unwrap();
        let imported = report
            .objects
            .iter()
            .find(|r| r.kind == "session" && r.id == "old")
            .unwrap();
        if case == "revert_before_summary" {
            assert_eq!(imported.outcome, Outcome::Deferred, "{report:?}");
            assert!(imported.target_id.is_none());
            assert_eq!(snapshot(&source), before);
            plugin.dispose().await.unwrap();
            root.shutdown().await.unwrap();
            continue;
        }
        assert_eq!(imported.outcome, Outcome::Imported, "{case}: {report:?}");
        let id = imported.target_id.clone().unwrap();
        plugin.dispose().await.unwrap();
        root.shutdown().await.unwrap();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install(&mut builder);
        defaults::mock_hosts(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                startup_timeout: std::time::Duration::from_secs(60),
                database_path: Some(target.join("rust/dimcode.sqlite")),
                product_home: Some(target),
                unix_path: Some(directory.path().join("native.sock")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let agent = server.agent().unwrap();
        let config = ConnectionConfig {
            endpoint: server.unix_path.clone().unwrap(),
            server: Server {
                program: std::env::current_exe().unwrap(),
                embedded: false,
            },
            profile: None,
            client_kind: "cli",
            client_name: "migration-test",
        };
        let mut client = Client::connect(&config).await.unwrap();
        client
            .rpc("session.attach", json!({"sessionId":id}))
            .await
            .unwrap();
        let session = agent.get_session(&id).unwrap();
        let history = session.history("main", None).await.unwrap();
        assert!(history.iter().all(|event| !matches!(
            event.kind,
            EventKind::ModelCall(_)
                | EventKind::ToolCall(_)
                | EventKind::ModelStart(_)
                | EventKind::ToolStart(_)
        )));
        if case == "tools" {
            assert!(history.iter().any(|event|matches!(&event.kind,EventKind::ToolResult(result) if result.results[0].result.structured_content.as_ref().and_then(|value|value.get("exitCode"))==Some(&json!(0)))));
        } else {
            assert!(history.iter().any(|event|matches!(&event.kind,EventKind::Text(text) if text.delta=="effective suffix")));
        }
        client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"continue"}),
            )
            .await
            .unwrap();
        client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await
            .unwrap();
        let history = session.history("main", None).await.unwrap();
        let request = history
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(call) => Some(&call.request),
                _ => None,
            })
            .unwrap();
        let context = serde_json::to_string(&request.messages).unwrap();
        match case {
            "tools" => {
                assert!(
                    request
                        .messages
                        .iter()
                        .any(|message| message.role == dim_agent_types::Role::Assistant)
                );
                assert!(
                    request
                        .messages
                        .iter()
                        .any(|message| message.role == dim_agent_types::Role::ToolResult)
                );
                assert!(
                    context.contains("old-call") && context.contains("historical output"),
                    "{context}"
                );
            }
            "revert" => {
                assert!(
                    !context.contains("compressed original")
                        && context.contains("retained summary")
                        && !context.contains("effective suffix"),
                    "{context}"
                );
            }
            "retry" => {
                assert!(
                    context.contains("retained summary") && !context.contains("effective suffix"),
                    "{context}"
                );
            }
            _ => unreachable!(),
        }
        assert!(
            history
                .iter()
                .all(|event| !matches!(event.kind, EventKind::ToolCall(_)))
        );
        assert_eq!(snapshot(&source), before);
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_migration_configuration_is_consumed_by_unchanged_web_and_scheduler() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        directory.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let source = directory.path().join("source");
    let db = fixture(&source);
    std::fs::create_dir_all(source.join("data/plugins/web")).unwrap();
    std::fs::write(source.join("data/plugins/web/state.json"),json!({"version":1,"enabled":false,"activeProvider":"brave","activeProviderExplicit":true,"providers":{"brave":{"apiKey":"private-brave-key"}}}).to_string()).unwrap();
    db.execute_batch("CREATE TABLE scheduled_jobs(jobId TEXT PRIMARY KEY,name TEXT,prompt TEXT,status TEXT,rrule TEXT,cwd TEXT,providerId TEXT,modelId TEXT,reasoningEffort TEXT,sessionMode TEXT,boundSessionId TEXT,nextRunAt TEXT,lastRunAt TEXT,createdAt TEXT);").unwrap();
    db.execute("INSERT INTO scheduled_jobs VALUES ('paused','Paused job','do work','paused','FREQ=DAILY;BYHOUR=9;BYMINUTE=15',?1,'mock','deterministic',NULL,'new',NULL,NULL,NULL,'2026-01-01T00:00:00Z')",[source.to_str().unwrap()]).unwrap();
    let before = snapshot(&source);
    let target = directory.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let root = rutis::Ctx::root().unwrap();
    let plugin = root.plugin(MigrationPlugin::new(
        target.clone(),
        "rust".into(),
        vec!["system".into()],
    ));
    (&plugin).await.unwrap();
    let report = root
        .get::<Migration>()
        .unwrap()
        .run(source.clone(), true, None)
        .await
        .unwrap();
    assert!(
        report
            .objects
            .iter()
            .any(|r| r.id == "web.enabled" && r.outcome == Outcome::Imported),
        "{report:?}"
    );
    assert!(
        report
            .objects
            .iter()
            .any(|r| r.id == "legacy-desktop:paused" && r.outcome == Outcome::Imported),
        "{report:?}"
    );
    plugin.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    let choices: Value =
        serde_json::from_slice(&std::fs::read(target.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(choices["names"]["web"]["off"], true);
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let defaults = target.join("defaults.json");
    std::fs::write(&defaults,json!({"plugins":{"scheduler":repository.join("runtime/plugins/scheduler")},"locked":["PluginManager","Rpc","Transport"]}).to_string()).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: std::time::Duration::from_secs(60),
            database_path: Some(target.join("rust/dimcode.sqlite")),
            product_home: Some(target),
            unix_path: Some(directory.path().join("native.sock")),
            plugin_defaults: Some(defaults),
            session_typescript: Some(
                dim_app_server::session_typescript::SessionTypeScriptConfig {
                    node: std::env::var_os("DIM_TEST_NODE").unwrap().into(),
                    js_runtime: std::env::var_os("DIM_TEST_JS_RUNTIME").unwrap().into(),
                    anchor: repository.join("runtime/package.json"),
                    python: None,
                    acquire: None,
                    rows: vec![],
                },
            ),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let config = ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "migration-test",
    };
    let mut client = Client::connect(&config).await.unwrap();
    let jobs = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            match client.typed::<Value>("schedule.list", json!({})).await {
                Ok(jobs) => break jobs,
                Err(error)
                    if error
                        .downcast_ref::<dim_client::RpcError>()
                        .is_some_and(|error| error.code == -32601) =>
                {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("Native Scheduler rejected imported data: {error}"),
            }
        }
    })
    .await
    .expect("Native Scheduler must finish loading");
    assert_eq!(jobs["jobs"][0]["id"], "legacy-desktop:paused", "{jobs}");
    assert_eq!(jobs["jobs"][0]["enabled"], false, "{jobs}");
    assert_eq!(jobs["jobs"][0]["schedule"]["cron"], "15 9 * * *", "{jobs}");
    let history: Value = client
        .typed(
            "schedule.history",
            json!({"job_id":"legacy-desktop:paused"}),
        )
        .await
        .unwrap();
    assert_eq!(history["runs"], json!([]), "{history}");
    assert!(
        client
            .rpc("settings.read", json!({"source":"web","scope":"user"}))
            .await
            .is_err()
    );
    client
        .rpc("plugin.enable", json!({"name":"web"}))
        .await
        .unwrap();
    let web = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if let Ok(value) = client
                .typed::<Value>("settings.read", json!({"source":"web","scope":"user"}))
                .await
            {
                break value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(web["document"]["searchService"]["kind"], "brave", "{web}");
    assert_eq!(snapshot(&source), before);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

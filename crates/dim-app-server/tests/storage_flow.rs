use dim_agent_capabilities::history::{HistoryRenderer, read_context};
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, AgentError, SessionConfig, StoredEvent, Timeline, TimelineKey,
    TimelineStore, TimelineTrace, TraceStep, capability_plugin,
};
use dim_agent_types::*;
use dim_app_server::storage::{NodeLoads, SqliteStorage, SqliteTimelineStore};
use rusqlite::{Connection, params};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

const INTERPRETER: &str = "dim/context-projection/v3";
fn snapshot(sequence: u64, checkpoint: &ProjectionCheckpoint) -> RecoverySnapshot {
    let mut snapshot = RecoverySnapshot {
        sequence,
        event_id: format!("event-{sequence}"),
        interpreter: INTERPRETER.into(),
        state: String::new(),
    };
    snapshot.state = (HistoryRenderer::default().encode_checkpoint)(&snapshot, checkpoint).unwrap();
    snapshot
}

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/storage-flow")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory.join("dimcode.sqlite"))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}
/// A completed compaction replacing everything before `retain_from` with one
/// summary message.
fn completed(retain_from: &str, summary: &str, through: u64) -> ContextCompactionData {
    ContextCompactionData {
        turn_id: Some("completed-turn".into()),
        request_id: Some("completed-request".into()),
        command: None,
        start: None,
        through,
        outcome: ContextCompactionResult::Completed {
            retain_from: retain_from.into(),
            replacement: vec![dim_agent_capabilities::history::summary_message(summary)],
            strategy: "summary".into(),
            diagnostics: serde_json::from_value(serde_json::json!({
                "reason": "auto", "trigger": "threshold", "estimatedInputTokens": 1,
                "entriesReplaced": 1, "modelCallCount": 1, "durationMs": 1,
                "strategy": {"kind": "summary"}
            }))
            .unwrap(),
            context_baseline: vec![],
        },
    }
}

fn key(session: &str, branch: &str) -> TimelineKey {
    TimelineKey {
        session_id: session.into(),
        branch_id: branch.into(),
    }
}
fn record(sequence: u64, kind: EventKind) -> StoredEvent {
    StoredEvent {
        event: Arc::new(AgentEvent {
            id: format!("event-{sequence}"),
            sequence,
            create_at: 1000 + sequence,
            tags: Some(vec!["stored".into()]),
            kind,
        }),
        trace: None,
        batch_end: sequence,
    }
}
fn text(sequence: u64, delta: &str) -> StoredEvent {
    record(
        sequence,
        EventKind::Text(DeltaData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            delta: delta.into(),
        }),
    )
}
async fn append(
    store: &SqliteTimelineStore,
    key: TimelineKey,
    records: Vec<StoredEvent>,
) -> dim_agent_runtime::Result<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    store.append(
        key,
        records,
        Box::new(move |result| {
            let _ = sender.send(result);
        }),
    )?;
    tokio::time::timeout(PUBLISHED, receiver)
        .await
        .unwrap()
        .unwrap()
}
/// How long a flow waits for the writer to publish a write before it fails
/// instead of hanging.
const PUBLISHED: Duration = Duration::from_secs(5);
/// Queues `records` and returns their publish result, which fails the test
/// after `PUBLISHED`.
fn queue(
    store: &SqliteTimelineStore,
    key: TimelineKey,
    records: Vec<StoredEvent>,
) -> impl Future<Output = dim_agent_runtime::Result<()>> + use<> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    store
        .append(
            key,
            records,
            Box::new(move |result| {
                let _ = sender.send(result);
            }),
        )
        .unwrap();
    async move {
        tokio::time::timeout(PUBLISHED, receiver)
            .await
            .expect("the writer did not publish the write")
            .unwrap()
    }
}
/// Appends `records` and holds the writer thread in their publish callback,
/// after their round committed, until the returned sender drops, which a
/// failing test's unwinding also does. The batches queued meanwhile all meet
/// in the writer's next round.
async fn hold_writer(
    store: &SqliteTimelineStore,
    key: TimelineKey,
    records: Vec<StoredEvent>,
) -> std::sync::mpsc::Sender<()> {
    let (held, holding) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    store
        .append(
            key,
            records,
            Box::new(move |result| {
                let _ = held.send(result);
                let _ = released.recv();
            }),
        )
        .unwrap();
    tokio::time::timeout(PUBLISHED, holding)
        .await
        .expect("the writer did not publish the write")
        .unwrap()
        .unwrap();
    release
}

/// The tables of the documented record example that hold business rows, in
/// an order foreign keys accept.
const BUSINESS_TABLES: &[&str] = &[
    "settings_documents",
    "providers",
    "provider_selections",
    "provider_imports",
    "agents",
    "agent_sessions",
    "session_owners",
    "timeline_branches",
    "timeline_content",
    "timeline_events",
    "timeline_snapshots",
    "session_files",
    "session_file_owners",
    "session_metadata",
];
/// The business tables each upgrade step adds: v15's, then v16's; v17 and
/// v18 add none.
const V15_TABLES: [&str; 2] = ["session_files", "session_file_owners"];
const V16_TABLES: [&str; 1] = ["session_metadata"];
/// Inserts the rows of `tables` from the complete record example in
/// native-storage-format.md.
fn insert_documented_records(connection: &Connection, tables: &[&str]) {
    use rusqlite::{params_from_iter, types::Value as SqlValue};
    let document = include_str!("../../../docs/design/appserver/native-storage-format.md");
    let json = document
        .split_once("## 完整记录示例")
        .unwrap()
        .1
        .split_once("```json\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    let example: serde_json::Value = serde_json::from_str(json).unwrap();
    for table in tables {
        for row in example[table].as_array().unwrap() {
            let row = row.as_object().unwrap();
            let columns = row.keys().cloned().collect::<Vec<_>>();
            let values = row
                .values()
                .map(|value| match value {
                    serde_json::Value::Null => SqlValue::Null,
                    serde_json::Value::Number(value) => SqlValue::Integer(value.as_i64().unwrap()),
                    serde_json::Value::String(value) => SqlValue::Text(value.clone()),
                    value => SqlValue::Text(value.to_string()),
                })
                .collect::<Vec<_>>();
            connection
                .execute(
                    &format!(
                        "INSERT INTO {table} ({}) VALUES ({})",
                        columns.join(","),
                        vec!["?"; columns.len()].join(",")
                    ),
                    params_from_iter(values),
                )
                .unwrap();
        }
    }
}

/// The v13 schema exactly as v13 daemons wrote it, frozen here so that tests
/// can build disposable v13 stores after the daemon moved on.
const V13_SCHEMA: &[&str] = &[
    "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 13))",
    "CREATE TABLE settings_documents (source TEXT NOT NULL PRIMARY KEY, version INTEGER NOT NULL CHECK(version > 0), document TEXT NOT NULL, imported_revision TEXT NOT NULL)",
    "CREATE TABLE provider_imports (kind TEXT NOT NULL, id TEXT NOT NULL, source_schema TEXT NOT NULL, source_version INTEGER NOT NULL, source_digest TEXT NOT NULL, PRIMARY KEY (kind, id))",
    "CREATE TABLE providers (provider_id TEXT NOT NULL PRIMARY KEY, version INTEGER NOT NULL CHECK(version > 0), record TEXT NOT NULL)",
    "CREATE TABLE provider_selections (scope TEXT NOT NULL PRIMARY KEY, version INTEGER NOT NULL CHECK(version > 0), provider_id TEXT NOT NULL, model_id TEXT NOT NULL)",
    "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0))",
    "CREATE TABLE agents(agent_id TEXT PRIMARY KEY, profile TEXT NOT NULL, is_default INTEGER NOT NULL CHECK(is_default IN (0, 1)))",
    "CREATE UNIQUE INDEX agents_default_unique ON agents(is_default) WHERE is_default = 1",
    "CREATE TABLE session_owners(session_id TEXT PRIMARY KEY REFERENCES agent_sessions(session_id), agent_id TEXT NOT NULL REFERENCES agents(agent_id), protocol_version INTEGER NOT NULL CHECK(protocol_version IN (1, 2, 3)))",
    "CREATE TABLE timeline_branches (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, info TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence >= 0), PRIMARY KEY (session_id, branch_id), FOREIGN KEY (session_id) REFERENCES agent_sessions(session_id))",
    "CREATE TABLE timeline_content (session_id TEXT NOT NULL, hash TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY (session_id, hash), FOREIGN KEY (session_id) REFERENCES agent_sessions(session_id))",
    "CREATE TABLE timeline_events (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), last_sequence INTEGER NOT NULL CHECK (last_sequence >= sequence), kind TEXT NOT NULL, body TEXT NOT NULL, event_id TEXT, PRIMARY KEY (session_id, branch_id, sequence), FOREIGN KEY (session_id, branch_id) REFERENCES timeline_branches(session_id, branch_id))",
    "CREATE TABLE timeline_snapshots (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), interpreter TEXT NOT NULL, event_id TEXT NOT NULL, state TEXT NOT NULL, PRIMARY KEY (session_id, branch_id, interpreter, sequence), FOREIGN KEY (session_id, branch_id, sequence) REFERENCES timeline_events(session_id, branch_id, sequence))",
    "CREATE INDEX session_owners_by_agent ON session_owners(agent_id, protocol_version, session_id)",
    "CREATE INDEX timeline_events_by_end ON timeline_events(session_id, branch_id, last_sequence, sequence)",
    "CREATE INDEX timeline_events_by_id ON timeline_events(session_id, event_id) WHERE event_id IS NOT NULL",
    "CREATE INDEX timeline_tool_use_starts ON timeline_events(session_id, json_extract(body, '$.record.event.data.request_id'), json_extract(body, '$.record.event.data.call_id')) WHERE branch_id = 'main' AND kind = 'step/tool-use-start'",
];
/// What v15 adds to v13 besides the marker, frozen likewise.
const V15_ADDITIONS: &[&str] = &[
    "CREATE TABLE session_files (file_id TEXT NOT NULL PRIMARY KEY, node TEXT, writer TEXT)",
    "CREATE TABLE session_file_owners (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), file_id TEXT NOT NULL REFERENCES session_files(file_id), PRIMARY KEY (session_id, file_id))",
    "CREATE INDEX session_file_owners_by_file ON session_file_owners(file_id)",
];
/// What v16 adds to v15 besides the marker.
const V16_ADDITIONS: &[&str] = &[
    "CREATE TABLE session_metadata (session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id), revision INTEGER NOT NULL CHECK (revision > 0), title TEXT, title_source TEXT CHECK (title_source IN ('auto', 'manual')), CHECK ((title IS NULL) = (title_source IS NULL)))",
];
/// The exact schema of `version`, sorted: v13's, plus each later step's
/// additions, with that version's marker.
fn schema_of(version: u32) -> Vec<String> {
    let mut statements: Vec<String> = V13_SCHEMA
        .iter()
        .chain(V15_ADDITIONS.iter().filter(|_| version >= 15))
        .chain(V16_ADDITIONS.iter().filter(|_| version >= 16))
        .map(|sql| sql.replace("(version = 13)", &format!("(version = {version})")))
        .collect();
    statements.sort();
    statements
}
/// Builds a disposable v13 store, as a v13 daemon created it.
fn v13_store(path: &std::path::Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    for sql in V13_SCHEMA {
        connection.execute_batch(sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (13)", [])
        .unwrap();
    connection
}
/// Builds a disposable store of v16's tables under `marker`, as a v16 daemon
/// created it, or a v17 daemon, which added no table.
fn v16_tables_store(path: &std::path::Path, marker: u32) -> Connection {
    let connection = Connection::open(path).unwrap();
    for sql in V13_SCHEMA.iter().chain(V15_ADDITIONS).chain(V16_ADDITIONS) {
        connection
            .execute_batch(&sql.replace("(version = 13)", &format!("(version = {marker})")))
            .unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (?1)", [marker])
        .unwrap();
    connection
}
/// Builds a disposable v15 store, as a v15 daemon created it.
fn v15_store(path: &std::path::Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    for sql in V13_SCHEMA.iter().chain(V15_ADDITIONS) {
        connection
            .execute_batch(&sql.replace("(version = 13)", "(version = 15)"))
            .unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (15)", [])
        .unwrap();
    connection
}
/// The schema statements of a store, sorted.
fn schema(connection: &Connection) -> Vec<String> {
    let mut statements: Vec<String> = connection
        .prepare("SELECT sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    statements.sort();
    statements
}
/// Every row of `table`, rendered and sorted.
fn rows(connection: &Connection, table: &str) -> Vec<String> {
    let mut query = connection
        .prepare(&format!("SELECT * FROM {table}"))
        .unwrap();
    let columns = query.column_count();
    let mut rows: Vec<String> = query
        .query_map([], |row| {
            (0..columns)
                .map(|index| {
                    row.get::<_, rusqlite::types::Value>(index)
                        .map(|value| format!("{value:?}"))
                })
                .collect::<rusqlite::Result<Vec<_>>>()
                .map(|values| values.join("|"))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    rows.sort();
    rows
}

/// Rewrites the request rows of `session` in the v16 shape, as a v16 daemon
/// wrote them: a `message` content row and reference per message instead of
/// a list. Rows without a before trace only.
fn as_v16_rows(connection: &Connection, session: &str) {
    use sha2::{Digest, Sha256};
    #[derive(serde::Deserialize)]
    struct Node {
        parent: Option<String>,
        message: Message,
    }
    let rows: Vec<(String, i64, String)> = connection
        .prepare("SELECT branch_id, sequence, body FROM timeline_events WHERE session_id = ?1 AND kind IN ('context/prepare', 'model/call')")
        .unwrap()
        .query_map([session], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (branch, sequence, body) in rows {
        let row: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(row["record"]["trace"].is_null());
        let refs = &row["record"]["refs"];
        let json = |value: &serde_json::Value| serde_json::to_string(value).unwrap();
        let mut messages = vec![];
        let mut next = refs["list"].as_str().map(str::to_owned);
        while let Some(address) = next {
            let node: String = connection
                .query_row(
                    "SELECT body FROM timeline_content WHERE session_id = ?1 AND hash = ?2",
                    [session, &address],
                    |row| row.get(0),
                )
                .unwrap();
            let node: Node = serde_json::from_str(&node).unwrap();
            let body = serde_json::to_string(&node.message).unwrap();
            let hash = format!("{:x}", Sha256::digest(format!("message\0{body}")));
            connection
                .execute(
                    "INSERT OR IGNORE INTO timeline_content VALUES (?1, ?2, 'message', ?3)",
                    [session, &hash, &body],
                )
                .unwrap();
            messages.push(hash);
            next = node.parent;
        }
        messages.reverse();
        let list = format!(
            r#""refs":{{"list":{},"length":{},"tools":{},"system":{}}}"#,
            json(&refs["list"]),
            refs["length"],
            json(&refs["tools"]),
            json(&refs["system"])
        );
        let references = format!(
            r#""refs":{{"messages":{},"tools":{},"system":{}}}"#,
            serde_json::to_string(&messages).unwrap(),
            json(&refs["tools"]),
            json(&refs["system"])
        );
        assert_eq!(body.matches(&list).count(), 1);
        connection
            .execute(
                "UPDATE timeline_events SET body = ?1 WHERE session_id = ?2 AND branch_id = ?3 AND sequence = ?4",
                params![body.replace(&list, &references), session, branch, sequence],
            )
            .unwrap();
    }
    connection
        .execute(
            "DELETE FROM timeline_content WHERE session_id = ?1 AND kind = 'node'",
            [session],
        )
        .unwrap();
}

#[tokio::test]
async fn documented_native_records_restore_with_legacy_snapshot_bypassed() {
    use dim_agent_capabilities::history::{
        project_context_history, project_history, projection_checkpoint,
    };
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();
    connection
        .execute("DELETE FROM agent_store_format", [])
        .unwrap();
    insert_documented_records(&connection, &["agent_store_format"]);
    insert_documented_records(&connection, BUSINESS_TABLES);
    drop(connection);

    assert_eq!(store.branches("s1").await.unwrap().len(), 2);
    let main = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("s1", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    let generated = projection_checkpoint(&main.context(2).await.unwrap(), "e2").unwrap();
    let saved = store
        .latest_snapshot(&key("s1", "main"), 7, "dim/context-projection/v1")
        .await
        .unwrap()
        .unwrap();
    let saved: ProjectionCheckpoint = serde_json::from_str(&saved.state).unwrap();
    assert_eq!(saved, generated);
    let full = project_history(&main.history(Some(7)).await.unwrap()).unwrap();
    let recovered = read_context(&main, 7).await.unwrap();
    assert!(recovered.checkpoint.is_none());
    let compacted = project_context_history(&recovered).unwrap();
    assert_eq!(
        full.iter().map(|entry| &entry.id).collect::<Vec<_>>(),
        ["e2", "e7"]
    );
    assert_eq!(compacted, full);
    let child = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("s1", "child"),
        vec![("main".into(), 7)],
        store.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        project_context_history(&read_context(&child, 2).await.unwrap()).unwrap(),
        compacted
    );

    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = builder.build();
    let session = agent.restore_session("s1".into()).await.unwrap();
    assert!(session.sequence("child").unwrap() >= 2);
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn committed_history_round_trips_requests_outputs_traces_and_branch_directory() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert!(
        SqliteStorage::open(database.0.clone()).is_err(),
        "only one process writer may own the database"
    );
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let mut request = ModelRequest::new(
        "mock",
        "test",
        vec![Message::text(Role::User, "shared message")],
    );
    request.tools = Some(Arc::new(vec![serde_json::from_value(serde_json::json!({"name":"echo", "description":"echo", "inputSchema":{"type":"object"}, "source":"static"})).unwrap()]));
    request.system = Some(Arc::new(SystemPrompt::Text("shared system".into())));
    let request = Arc::new(request);
    let prepare = EventKind::ContextPrepare(ContextPrepareData {
        turn_id: "turn".into(),
        request_id: "request".into(),
        through: 0,
        request: request.clone(),
        configuration: None,
        prompt: None,
        recovery: None,
    });
    let call = EventKind::ModelCall(ModelCallData {
        turn_id: "turn".into(),
        request_id: "request".into(),
        request,
        configuration: None,
        prompt: None,
    });
    let mut first = record(1, prepare.clone());
    first.batch_end = 2;
    first.trace = Some(TimelineTrace {
        event_id: first.event.id.clone(),
        original: EventDraft::new(prepare.clone()),
        steps: vec![TraceStep {
            handler: "before".into(),
            result: Some(EventDraft {
                tags: first.event.tags.clone(),
                kind: prepare,
            }),
        }],
    });
    let second = record(2, call);
    let mut expected = vec![first.clone(), second.clone()];
    append(&store, key("session", "main"), expected.clone())
        .await
        .unwrap();
    append(&store, key("session", "main"), vec![text(3, "a")])
        .await
        .unwrap();
    expected.push(text(3, "a"));
    let mut fourth = text(4, "b");
    fourth.batch_end = 5;
    let mut fifth = text(5, "c");
    fifth.trace = Some(TimelineTrace {
        event_id: fifth.event.id.clone(),
        original: EventDraft::new(EventKind::Text(DeltaData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            delta: "before".into(),
        })),
        steps: vec![TraceStep {
            handler: "transform".into(),
            result: Some(EventDraft {
                tags: fifth.event.tags.clone(),
                kind: fifth.event.kind.clone(),
            }),
        }],
    });
    append(
        &store,
        key("session", "main"),
        vec![fourth.clone(), fifth.clone()],
    )
    .await
    .unwrap();
    expected.extend([fourth, fifth]);
    assert!(matches!(
        store.read(&key("session", "main"), 3..=4).await,
        Err(AgentError::InvalidInput(_))
    ));
    assert!(store.read(&key("session", "main"), 1..=1).await.is_err());
    assert_eq!(
        store
            .read(&key("session", "main"), 2..=2)
            .await
            .unwrap()
            .len(),
        1
    );
    let branch = record(
        6,
        EventKind::BranchCreate(BranchCreateData {
            at: BranchCreateAt::Sequence { sequence: 2 },
            source: BranchSource::Host {
                operation_id: "fork".into(),
            },
            branch: "child".into(),
            prompt: "child prompt".into(),
        }),
    );
    append(&store, key("session", "main"), vec![branch.clone()])
        .await
        .unwrap();
    expected.push(branch);
    let mut child = record(
        1,
        EventKind::InputMessage(InputMessageData::text("child", None)),
    );
    Arc::make_mut(&mut child.event).id = "child-event-1".into();
    append(&store, key("session", "child"), vec![child.clone()])
        .await
        .unwrap();
    let mut child_request = second.clone();
    Arc::make_mut(&mut child_request.event).id = "child-event-2".into();
    append(&store, key("session", "child"), vec![child_request])
        .await
        .unwrap();
    for sequence in [3, 4] {
        let mut thinking = record(
            sequence,
            EventKind::Thinking(DeltaData {
                turn_id: "child-turn".into(),
                request_id: "child-request".into(),
                delta: "thought".into(),
            }),
        );
        Arc::make_mut(&mut thinking.event).id = format!("child-event-{sequence}");
        append(&store, key("session", "child"), vec![thinking])
            .await
            .unwrap();
    }
    for entry in [
        text(7, "after branch"),
        text(8, &"x".repeat(4100)),
        text(9, "after size"),
        {
            let mut entry = text(10, "after time");
            Arc::make_mut(&mut entry.event).create_at = 3000;
            entry
        },
    ] {
        append(&store, key("session", "main"), vec![entry.clone()])
            .await
            .unwrap();
        expected.push(entry);
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM timeline_content", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_events WHERE kind = 'step/text'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_events WHERE kind = 'step/thinking'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let branches = store.branches("session").await.unwrap();
    let child_info = &branches
        .iter()
        .find(|branch| branch.info.id == "child")
        .unwrap()
        .info;
    assert_eq!(child_info.parent.as_deref(), Some("main"));
    assert_eq!(child_info.branch_point, Some(2));
    assert_eq!(child_info.create_anchor, Some(6));
    let restored = store.read(&key("session", "main"), 1..=10).await.unwrap();
    for (actual, expected) in restored.iter().zip(&expected) {
        assert_eq!(actual.event, expected.event);
        assert_eq!(actual.batch_end, expected.batch_end);
        assert_eq!(
            serde_json::to_value(&actual.trace).unwrap(),
            serde_json::to_value(&expected.trace).unwrap()
        );
    }
    let EventKind::ContextPrepare(prepare) = &restored[0].event.kind else {
        panic!()
    };
    let EventKind::ModelCall(call) = &restored[1].event.kind else {
        panic!()
    };
    let segments = store
        .read_segments(
            "session",
            vec![("main".into(), 1..=2), ("child".into(), 2..=2)],
        )
        .await
        .unwrap();
    let EventKind::ContextPrepare(ancestor_request) = &segments[0][0].event.kind else {
        panic!()
    };
    let EventKind::ModelCall(child_request) = &segments[1][0].event.kind else {
        panic!()
    };
    assert!(Arc::ptr_eq(
        &ancestor_request.request.messages[0],
        &child_request.request.messages[0]
    ));
    assert!(Arc::ptr_eq(
        &prepare.request.messages[0],
        &call.request.messages[0]
    ));
    assert!(Arc::ptr_eq(
        prepare.request.tools.as_ref().unwrap(),
        call.request.tools.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        prepare.request.system.as_ref().unwrap(),
        call.request.system.as_ref().unwrap()
    ));
    assert_eq!(
        store.read(&key("session", "child"), 1..=1).await.unwrap()[0].event,
        child.event
    );
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    assert_eq!(timeline.sequence(), 10);
    assert_eq!(timeline.history(None).await.unwrap().len(), 10);
    timeline
        .append(EventDraft::new(EventKind::StopInterrupt(
            StopInterruptData {
                reason: "filler".into(),
                scope: InterruptScope::Turn,
            },
        )))
        .unwrap();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    assert_eq!(store.head(&key("session", "main")).await.unwrap(), 11);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// An extension event (#2) is stored and read like any other event: it gets a
/// row of its own holding its data's bytes, so the output deltas around it are
/// not merged across it, and a restored Session, whose TimelineTree reads each
/// branch's history once as it opens, decodes it unchanged.
#[tokio::test]
async fn extension_events_keep_their_own_rows_and_restore_unchanged() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let data = serde_json::json!({"z": 1, "a": [true, null], "nested": {"y": "x"}});
    let extension = ExtensionEvent::new("myplugin", "checkpoint", data.clone()).unwrap();
    let written = [
        text(1, "a"),
        record(2, EventKind::Extension(extension)),
        text(3, "b"),
    ];
    for entry in &written {
        append(&store, key("session", "main"), vec![entry.clone()])
            .await
            .unwrap();
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    let rows = connection
        .prepare(
            "SELECT sequence, last_sequence, kind, body FROM timeline_events ORDER BY sequence",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|(sequence, last, kind, _)| (*sequence, *last, kind.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, 1, "step/text"),
            (2, 2, "myplugin/checkpoint"),
            (3, 3, "step/text")
        ]
    );
    assert!(rows[1].3.contains(&serde_json::to_string(&data).unwrap()));
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = builder.build();
    let session = agent.restore_session("session".into()).await.unwrap();
    let restored = session.history("main", Some(3)).await.unwrap();
    let written: Vec<_> = written.iter().map(|entry| entry.event.clone()).collect();
    assert_eq!(
        serde_json::to_string(&restored).unwrap(),
        serde_json::to_string(&written).unwrap()
    );
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// A history whose requests cover every list case: a new list after a
/// compaction, a request keeping part and one keeping all of the previous
/// one, `tools` and `system` absent, reused and changed, request drafts, and
/// lists shared from a parent and a grandparent while the parent writes past
/// the cut. One batch per entry, in write order.
fn request_history() -> Vec<(&'static str, StoredEvent)> {
    let text = |role, text: &str| Message::text(role, text);
    let tools = |names: &[&str]| -> Arc<Vec<ToolDefinition>> {
        Arc::new(
            names
                .iter()
                .map(|name| {
                    serde_json::from_value(serde_json::json!({"name": name, "description": name, "inputSchema": {"type": "object"}, "source": "static"}))
                        .unwrap()
                })
                .collect(),
        )
    };
    let request = |messages: Vec<Message>, system: Option<&str>, names: Option<&[&str]>| {
        let mut request = ModelRequest::new("mock", "test", messages);
        request.system = system.map(|system| Arc::new(SystemPrompt::Text(system.into())));
        request.tools = names.map(tools);
        Arc::new(request)
    };
    let prepare = |request: &Arc<ModelRequest>| {
        EventKind::ContextPrepare(ContextPrepareData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            through: 0,
            request: request.clone(),
            configuration: None,
            prompt: None,
            recovery: None,
        })
    };
    let call = |request: &Arc<ModelRequest>| {
        EventKind::ModelCall(ModelCallData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            request: request.clone(),
            configuration: None,
            prompt: None,
        })
    };
    let created = |branch: &str, at| {
        EventKind::BranchCreate(BranchCreateData {
            at,
            source: BranchSource::Host {
                operation_id: branch.into(),
            },
            branch: branch.into(),
            prompt: String::new(),
        })
    };
    let started = |parent: &str, create_event| {
        EventKind::BranchStart(BranchStartData {
            parent: Some(parent.into()),
            create_event: Some(create_event),
        })
    };
    let (user, assistant) = (Role::User, Role::Assistant);
    let first = request(vec![text(user, "u1")], Some("one"), Some(&["echo"]));
    // Equal values in new allocations: reused by `==`, not by pointer.
    let second = request(
        vec![text(user, "u1"), text(assistant, "a1"), text(user, "u2")],
        Some("one"),
        Some(&["echo"]),
    );
    // An early message changed: keeps one message, found by stepping back.
    let third = request(
        vec![
            text(user, "u1"),
            text(assistant, "a1 revised"),
            text(user, "u2"),
            text(assistant, "a2"),
            text(user, "u3"),
        ],
        None,
        Some(&["echo", "grep"]),
    );
    // After a compaction: a new list.
    let fourth = request(
        vec![
            text(user, "summary"),
            text(assistant, "b1"),
            text(user, "u3"),
        ],
        Some("two"),
        Some(&["echo", "grep"]),
    );
    let child = request(
        vec![
            text(user, "u1"),
            text(assistant, "a1"),
            text(user, "u2"),
            text(assistant, "child a2"),
            text(user, "child u3"),
        ],
        Some("one"),
        Some(&["echo"]),
    );
    let grandchild = request(
        vec![
            text(user, "u1"),
            text(assistant, "a1"),
            text(user, "u2"),
            text(assistant, "grandchild a2"),
        ],
        Some("one"),
        Some(&["echo"]),
    );
    let mut traced = record(2, prepare(&first));
    let draft = |request: &Arc<ModelRequest>| {
        Some(EventDraft {
            tags: traced.event.tags.clone(),
            kind: prepare(request),
        })
    };
    let steps = [
        ("unchanged", draft(&first)),
        (
            "appended",
            draft(&request(
                vec![text(user, "u1"), text(user, "appended")],
                Some("one"),
                Some(&["echo"]),
            )),
        ),
        (
            "rewritten",
            draft(&request(
                vec![text(user, "u1 rewritten")],
                Some("one"),
                Some(&["echo"]),
            )),
        ),
        (
            "system",
            draft(&request(
                vec![text(user, "u1")],
                Some("two"),
                Some(&["echo"]),
            )),
        ),
        ("dropped", None),
    ];
    traced.trace = Some(TimelineTrace {
        event_id: traced.event.id.clone(),
        original: EventDraft::new(prepare(&first)),
        steps: steps
            .into_iter()
            .map(|(handler, result)| TraceStep {
                handler: handler.into(),
                result,
            })
            .collect(),
    });
    let mut history = vec![
        (
            "main",
            record(
                1,
                EventKind::InputMessage(InputMessageData::text("u1", None)),
            ),
        ),
        ("main", traced),
        ("main", record(3, call(&first))),
        ("main", record(4, prepare(&second))),
        ("main", record(5, call(&second))),
        (
            "main",
            record(
                6,
                created("child", BranchCreateAt::Sequence { sequence: 5 }),
            ),
        ),
        ("main", record(7, prepare(&third))),
        ("main", record(8, call(&third))),
        ("child", record(1, started("main", 6))),
        ("child", record(2, prepare(&child))),
        ("child", record(3, call(&child))),
        (
            "child",
            record(
                4,
                created("grandchild", BranchCreateAt::Sequence { sequence: 1 }),
            ),
        ),
        ("grandchild", record(1, started("child", 4))),
        ("grandchild", record(2, prepare(&grandchild))),
        (
            "main",
            record(
                9,
                EventKind::ContextCompaction(completed("event-1", "summary", 8)),
            ),
        ),
        ("main", record(10, prepare(&fourth))),
        ("main", record(11, call(&fourth))),
    ];
    for (branch, record) in &mut history {
        if *branch != "main" {
            let event = Arc::make_mut(&mut record.event);
            event.id = format!("{branch}-{}", event.sequence);
            if let Some(trace) = &mut record.trace {
                trace.event_id = event.id.clone();
            }
        }
    }
    history
}
/// The rows of Session `session`, as stored.
fn session_rows(connection: &Connection, session: &str) -> (Vec<String>, Vec<String>) {
    let column = |sql: &str| -> Vec<String> {
        connection
            .prepare(sql)
            .unwrap()
            .query_map([session], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    (
        column(
            "SELECT branch_id || '|' || sequence || '|' || last_sequence || '|' || kind || '|' || body || '|' || ifnull(event_id, '') FROM timeline_events WHERE session_id = ?1 ORDER BY branch_id, sequence",
        ),
        column(
            "SELECT hash || '|' || kind || '|' || body FROM timeline_content WHERE session_id = ?1 ORDER BY hash",
        ),
    )
}

/// The writer encodes each request against the previous one in its
/// branch's logical history, whether that request was written earlier in
/// the same round, read back from the store after a restart or without a
/// writer memo, or in a round another Session's failed commit split. Every
/// way gives the same rows, and every event reads back as written.
#[tokio::test]
async fn request_lists_round_trip_with_the_same_bytes_however_writes_meet_rounds() {
    let history = request_history();
    let mut written = vec![];
    for way in [
        "rounds",
        "no memo",
        "one round",
        "restart",
        "failed neighbour",
    ] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let mut store = if way == "no memo" {
            SqliteTimelineStore::without_writer_memo(db.clone())
        } else {
            SqliteTimelineStore::new(db.clone())
        };
        for session in ["session", "bad"] {
            store
                .create_session(session, &SessionConfig::default())
                .await
                .unwrap();
        }
        let connection = Connection::open(&database.0).unwrap();
        match way {
            "rounds" | "no memo" => {
                for (branch, record) in &history {
                    append(&store, key("session", branch), vec![record.clone()])
                        .await
                        .unwrap();
                }
            }
            "one round" => {
                // The writer is held after a first round, so the rest queue
                // up for one round.
                let release = hold_writer(&store, key("bad", "main"), vec![text(1, "warm")]).await;
                let queued: Vec<_> = history
                    .iter()
                    .map(|(branch, record)| {
                        queue(&store, key("session", branch), vec![record.clone()])
                    })
                    .collect();
                drop(release);
                for published in queued {
                    published.await.unwrap();
                }
            }
            "restart" => {
                for (index, (branch, record)) in history.iter().enumerate() {
                    if index == 7 {
                        store.close().await.unwrap();
                        store = SqliteTimelineStore::new(db.clone());
                    }
                    append(&store, key("session", branch), vec![record.clone()])
                        .await
                        .unwrap();
                }
            }
            "failed neighbour" => {
                // The writer is held after the round of entry 2, so `bad`'s
                // write and entry 3 meet in one round. Its combined commit
                // fails on Session `bad`; `session` commits alone, and its
                // later writes continue from what committed.
                connection.execute_batch("CREATE TRIGGER fail_bad BEFORE INSERT ON timeline_events WHEN NEW.session_id = 'bad' BEGIN SELECT RAISE(ABORT, 'injected failure'); END").unwrap();
                let mut release = None;
                for (index, (branch, record)) in history.iter().enumerate() {
                    match index {
                        2 => {
                            release = Some(
                                hold_writer(&store, key("session", branch), vec![record.clone()])
                                    .await,
                            );
                        }
                        3 => {
                            let bad = queue(&store, key("bad", "main"), vec![text(1, "bad")]);
                            let good = queue(&store, key("session", branch), vec![record.clone()]);
                            drop(release.take());
                            assert!(bad.await.is_err());
                            good.await.unwrap();
                        }
                        _ => append(&store, key("session", branch), vec![record.clone()])
                            .await
                            .unwrap(),
                    }
                }
            }
            _ => unreachable!(),
        }
        for branch in ["main", "child", "grandchild"] {
            let expected: Vec<_> = history
                .iter()
                .filter(|(owner, _)| *owner == branch)
                .map(|(_, record)| record)
                .collect();
            let read = store
                .read(&key("session", branch), 1..=expected.len() as u64)
                .await
                .unwrap();
            assert_eq!(read.len(), expected.len());
            for (actual, expected) in read.iter().zip(expected) {
                assert_eq!(actual.event, expected.event);
                assert_eq!(actual.batch_end, expected.batch_end);
                assert_eq!(
                    serde_json::to_value(&actual.trace).unwrap(),
                    serde_json::to_value(&expected.trace).unwrap()
                );
            }
        }
        assert_eq!(store.branches("session").await.unwrap().len(), 3);
        written.push((way, session_rows(&connection, "session")));
        drop(connection);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    // Nodes: 1 for the first request; its drafts 1 appended and 1
    // rewritten; 2 for the second; 4 for the third, which keeps one; 3 for
    // the new list after the compaction; 2 for the child and 1 for the
    // grandchild, which share 3 from `main`. `tools` and `system` once per
    // distinct value; no `message` rows.
    let (_, content) = &written[0].1;
    let kinds = |kind: &str| {
        content
            .iter()
            .filter(|row| row.split('|').nth(1) == Some(kind))
            .count()
    };
    assert_eq!(
        [
            kinds("node"),
            kinds("tools"),
            kinds("system"),
            content.len()
        ],
        [15, 2, 2, 19]
    );
    for (way, rows) in &written[1..] {
        assert_eq!(rows, &written[0].1, "{way}");
    }
}

/// Only output deltas wait for a write round (timeline storage §3). A round
/// that begins with them waits until the window after the previous round
/// started, and any other batch or a close ends the wait; every other round
/// starts at once. A round takes every pending batch, each branch in
/// sequence order, and output rows hold the same bytes whether deltas meet
/// in one round or each commits in its own.
#[tokio::test]
async fn only_output_deltas_wait_for_a_round_until_another_batch_or_close() {
    // Twice `PUBLISHED`: a wait the writer should end or not begin holds a
    // write past its timeout and fails the test, which still ends.
    const WINDOW: Duration = Duration::from_secs(10);
    let thinking = |sequence, delta: &str| {
        record(
            sequence,
            EventKind::Thinking(DeltaData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                delta: delta.into(),
            }),
        )
    };
    let input = |sequence, text: &str| {
        record(
            sequence,
            EventKind::InputMessage(InputMessageData::text(text, None)),
        )
    };
    let mut pair = vec![text(4, "c"), text(5, "d")];
    pair[0].batch_end = 5;
    // In queue order: deltas of two Sessions, an input between them, and a
    // batch of two deltas.
    let batches = [
        ("s", vec![thinking(1, "a")]),
        ("t", vec![text(1, "x")]),
        ("s", vec![thinking(2, "b")]),
        ("s", vec![input(3, "u")]),
        ("t", vec![text(2, "y")]),
        ("s", pair),
    ];
    let heads = |connection: &Connection| -> Vec<String> {
        connection
            .prepare("SELECT session_id || '/' || sequence FROM timeline_branches WHERE session_id IN ('s', 't') ORDER BY session_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let mut written = vec![];
    for one_round in [false, true] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = if one_round {
            SqliteTimelineStore::with_round_window(db.clone(), WINDOW)
        } else {
            SqliteTimelineStore::new(db.clone())
        };
        for session in ["warm", "s", "t"] {
            store
                .create_session(session, &SessionConfig::default())
                .await
                .unwrap();
        }
        let connection = Connection::open(&database.0).unwrap();
        if one_round {
            append(&store, key("warm", "main"), vec![input(1, "first")])
                .await
                .unwrap();
            // Held after the next round, the writer finds every batch
            // pending. Each batch publishes with every other one already
            // durable: they committed in one round.
            let before = Instant::now();
            let release = hold_writer(&store, key("warm", "main"), vec![input(2, "held")]).await;
            let held = Instant::now();
            let published: Vec<_> = batches
                .iter()
                .map(|(session, records)| {
                    let (sender, receiver) = tokio::sync::oneshot::channel();
                    let path = database.0.clone();
                    store
                        .append(
                            key(session, "main"),
                            records.clone(),
                            Box::new(move |result| {
                                let durable = Connection::open(path).map(|probe| heads(&probe));
                                let _ = sender.send((result, durable));
                            }),
                        )
                        .unwrap();
                    receiver
                })
                .collect();
            drop(release);
            for receiver in published {
                let (result, durable) = tokio::time::timeout(PUBLISHED, receiver)
                    .await
                    .unwrap()
                    .unwrap();
                result.unwrap();
                assert_eq!(durable.unwrap(), ["s/5", "t/2"]);
            }
            // The held round, an input after a round, started at once. The
            // last one began with deltas, so it was to wait until the window
            // after the held round started, and the input ended the wait.
            let deadlines = store.take_round_deadlines();
            let [None, None, Some(deadline)] = deadlines[..] else {
                panic!("{deadlines:?}");
            };
            assert!(before + WINDOW <= deadline && deadline <= held + WINDOW);
        } else {
            for (session, records) in &batches {
                append(&store, key(session, "main"), records.clone())
                    .await
                    .unwrap();
            }
            // After the first round, each round of deltas waited for the
            // window and the input's round did not.
            let waited: Vec<_> = store
                .take_round_deadlines()
                .iter()
                .map(Option::is_some)
                .collect();
            assert_eq!(waited, [false, true, true, false, true, true]);
        }
        for (session, last) in [("s", 5), ("t", 2)] {
            let expected: Vec<_> = batches
                .iter()
                .filter(|(owner, _)| *owner == session)
                .flat_map(|(_, records)| records)
                .collect();
            let read = store.read(&key(session, "main"), 1..=last).await.unwrap();
            assert_eq!(read.len(), expected.len());
            for (actual, expected) in read.iter().zip(expected) {
                assert_eq!(actual.event, expected.event);
                assert_eq!(actual.batch_end, expected.batch_end);
            }
        }
        written.push([
            session_rows(&connection, "s"),
            session_rows(&connection, "t"),
        ]);
        // A delta waits again, and closing ends the wait: the close returns
        // once its round committed.
        let last = queue(&store, key("t", "main"), vec![text(3, "z")]);
        tokio::time::timeout(PUBLISHED, store.close())
            .await
            .unwrap()
            .unwrap();
        last.await.unwrap();
        assert!(matches!(store.take_round_deadlines()[..], [Some(_)]));
        drop(connection);
        db.close().await.unwrap();
    }
    assert_eq!(written[0], written[1]);
}

/// A model call at `sequence` whose request has `messages`.
fn model_call(sequence: u64, messages: Vec<Message>) -> StoredEvent {
    record(
        sequence,
        EventKind::ModelCall(ModelCallData {
            turn_id: format!("turn-{sequence}"),
            request_id: format!("request-{sequence}"),
            request: Arc::new(ModelRequest::new("mock", "test", messages)),
            configuration: None,
            prompt: None,
        }),
    )
}

/// §18 items 3 and 3a: a read loads each node it needs once, in connection
/// holds of at most 256 node loads. A page that starts mid-history rebuilds
/// only its first request's list, and a checkpoint read reads no row that
/// ends before the checkpoint's `resume`.
#[tokio::test]
async fn reads_rebuild_lists_from_where_they_start_in_bounded_holds() {
    use dim_agent_capabilities::history::{projection_checkpoint, summary_message};
    const TURNS: u64 = 300;
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("long", &SessionConfig::default())
        .await
        .unwrap();
    let main = key("long", "main");
    // Turn t: a queued input and its consumption in one batch, then at 3t a
    // model call whose request extends the previous one by that input.
    let mut messages = vec![];
    let mut records = vec![];
    for turn in 1..=TURNS {
        let input = 3 * turn - 2;
        messages.push(Message::text(Role::User, format!("u{turn}")));
        let mut batch = [
            record(
                input,
                EventKind::InputMessage(InputMessageData::text(
                    format!("u{turn}"),
                    Some(InputMode::Queue),
                )),
            ),
            record(
                input + 1,
                EventKind::InputConsumed(InputConsumedData {
                    input_id: format!("event-{input}"),
                }),
            ),
        ];
        for record in &mut batch {
            record.batch_end = input + 1;
        }
        records.extend(batch);
        records.push(model_call(input + 2, messages.clone()));
    }
    append(&store, main.clone(), records).await.unwrap();
    // A compaction keeps the last three turns, and the next call starts a
    // new list of four messages.
    let through = 3 * TURNS;
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        main.clone(),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    let retain_from = format!("event-{}", 3 * (TURNS - 2) - 2);
    let checkpoint =
        projection_checkpoint(&timeline.context(through).await.unwrap(), &retain_from).unwrap();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    let resume = checkpoint.resume.sequence;
    assert!(resume <= 3 * (TURNS - 2));
    let compaction = through + 1;
    let head = compaction + 1;
    let mut after_compaction = vec![summary_message("summary")];
    after_compaction.extend(messages[TURNS as usize - 3..].iter().cloned());
    append(
        &store,
        main.clone(),
        vec![
            record(
                compaction,
                EventKind::ContextCompaction(completed(&retain_from, "summary", through)),
            ),
            model_call(head, after_compaction),
        ],
    )
    .await
    .unwrap();
    store
        .save_snapshot(&main, snapshot(compaction, &checkpoint))
        .await
        .unwrap();
    store.close().await.unwrap();
    // Every node once, also without the content cache (a budget no body
    // fits in): the reader keeps the last request it rebuilt with its node
    // addresses. The TURNS nodes of the list before the compaction and the
    // four of the new list.
    let nodes = TURNS as usize + 4;
    for budget in [None, Some(0)] {
        let store = match budget {
            None => SqliteTimelineStore::new(db.clone()),
            Some(bytes) => SqliteTimelineStore::with_content_budget(db.clone(), bytes),
        };
        assert_eq!(
            store.read(&main, 1..=head).await.unwrap().len() as u64,
            head
        );
        let loads = store.take_node_loads();
        assert_eq!(loads.loads, nodes, "budget {budget:?}");
        assert!(loads.max_per_hold <= 256);
        // A validation pass reads the compaction's preparation boundary
        // again after its in-order pass; the content cache serves that.
        store.branches("long").await.unwrap();
        let validation = store.take_node_loads();
        if budget.is_none() {
            assert_eq!(validation.loads, nodes);
        }
        // A page from turn 280's model call, with both boundaries on input
        // batches: that call's list, then one node for each of the next
        // nine.
        let page = store
            .read_page(&main, 3 * 280 - 1, 30, 3 * 295 - 1)
            .await
            .unwrap();
        assert_eq!(page.len(), 30);
        assert_eq!(
            page[0].event,
            model_call(3 * 280, messages[..280].to_vec()).event
        );
        assert_eq!(
            store.take_node_loads(),
            NodeLoads {
                loads: 280 + 9,
                holds: 2,
                max_per_hold: 256,
                in_transaction: 0,
                encoded: 0,
            },
            "budget {budget:?}"
        );
        // The store's read from `resume`: its first request's list, one
        // node for each later call before the compaction, and the new list.
        let from_resume = store.read(&main, resume..=head).await.unwrap();
        assert_eq!(from_resume.len() as u64, head - resume + 1);
        let loads = store.take_node_loads();
        assert_eq!(loads.loads, nodes, "budget {budget:?}");
        assert!(loads.max_per_hold <= 256);
        store.close().await.unwrap();
    }
    let store = SqliteTimelineStore::new(db.clone());
    // A checkpoint read through the compaction gives the same context once
    // every row ending before `resume` is unreadable, so it read none.
    let context = || async {
        let timeline = Timeline::restore(
            rutis::Ctx::root().unwrap(),
            main.clone(),
            vec![],
            store.clone(),
        )
        .await
        .unwrap();
        let context = read_context(&timeline, head).await.unwrap();
        dim_agent_runtime::core::close_timeline(&timeline)
            .await
            .unwrap();
        (context.checkpoint, context.events, context.positions)
    };
    let before = context().await;
    assert_eq!(before.0.as_ref(), Some(&checkpoint));
    assert_eq!(before.2[0].sequence, resume);
    let connection = Connection::open(&database.0).unwrap();
    let overwritten = connection
        .execute(
            "UPDATE timeline_events SET body = '{' WHERE session_id = 'long' AND last_sequence < ?1",
            [resume as i64],
        )
        .unwrap();
    assert_eq!(overwritten as u64, resume - 1);
    assert!(store.branches("long").await.is_err());
    store.take_node_loads();
    let after = context().await;
    assert_eq!(after, before);
    assert!(store.take_node_loads().max_per_hold <= 256);
    drop(connection);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// §18 item 3b: one long request, append-only or with its last four
/// messages rewritten each turn. A cold read of it loads each of its nodes
/// once, in holds of at most 256 node loads. After a restart the first
/// request written on the branch rebuilds its previous request in the same
/// holds, before its write transaction, which loads no node. The running
/// Session's next request takes its previous request from the writer memo
/// and loads only the nodes it steps back over: none when it extends that
/// request, four when it rewrites the last four messages. A request after
/// another in the same round takes that one and loads no node. Every write
/// encodes nodes for its new messages only.
#[tokio::test]
async fn a_long_request_is_read_and_extended_in_bounded_holds() {
    const MESSAGES: usize = 600;
    for rewrite in [false, true] {
        let length = |turn: usize| if rewrite { MESSAGES } else { MESSAGES + turn };
        let call = |turn: usize| {
            let messages = (0..length(turn))
                .map(|index| {
                    let text = if rewrite && index + 4 >= MESSAGES {
                        format!("turn {turn} message {index}")
                    } else {
                        format!("message {index}")
                    };
                    Message::text(Role::User, text)
                })
                .collect();
            model_call(turn as u64, messages)
        };
        // Turn `turn`'s list loaded in full, and `encoded` new nodes.
        let holds = |turn: usize, encoded| NodeLoads {
            loads: length(turn),
            holds: length(turn).div_ceil(256),
            max_per_hold: 256,
            in_transaction: 0,
            encoded,
        };
        let fresh = if rewrite { 4 } else { 1 };
        let main = key("long", "main");
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("long", &SessionConfig::default())
            .await
            .unwrap();
        for turn in [1, 2] {
            append(&store, main.clone(), vec![call(turn)])
                .await
                .unwrap();
        }
        store.close().await.unwrap();
        db.close().await.unwrap();

        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let read = store.read(&main, 2..=2).await.unwrap();
        assert_eq!(read[0].event, call(2).event);
        assert_eq!(store.take_node_loads(), holds(2, 0), "rewrite: {rewrite}");
        let third = call(3);
        append(&store, main.clone(), vec![third.clone()])
            .await
            .unwrap();
        assert_eq!(
            store.take_node_loads(),
            holds(2, fresh),
            "rewrite: {rewrite}"
        );
        let stepped = if rewrite { 4 } else { 0 };
        let fourth = call(4);
        append(&store, main.clone(), vec![fourth.clone()])
            .await
            .unwrap();
        assert_eq!(
            store.take_node_loads(),
            NodeLoads {
                loads: stepped,
                holds: usize::from(rewrite),
                max_per_hold: stepped,
                in_transaction: 0,
                encoded: fresh,
            },
            "rewrite: {rewrite}"
        );
        store.close().await.unwrap();
        db.close().await.unwrap();

        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::without_writer_memo(db.clone());
        append(&store, main.clone(), vec![call(5), call(6)])
            .await
            .unwrap();
        assert_eq!(
            store.take_node_loads(),
            holds(4, 2 * fresh),
            "rewrite: {rewrite}"
        );
        let read = store.read(&main, 1..=6).await.unwrap();
        for (turn, record) in (1..=6).zip(&read) {
            assert_eq!(record.event, call(turn).event);
        }
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
}

/// The round's combined commit fails on Session `bad`, so the writer
/// retries each Session alone: `bad` fails and is closed to writes with its
/// directory unchanged, and the other Sessions of the round commit.
#[tokio::test]
async fn failed_session_rolls_back_directory_and_does_not_block_other_sessions() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    for session in ["warm", "bad", "good", "other"] {
        store
            .create_session(session, &SessionConfig::default())
            .await
            .unwrap();
    }
    // A request the runtime still holds when its Session's commit fails.
    let history = request_history();
    let request = record(1, history[1].1.event.kind.clone());
    append(&store, key("bad", "main"), vec![request])
        .await
        .unwrap();
    assert_eq!(store.writer_memo_requests("bad"), 1);
    let connection = Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_bad BEFORE INSERT ON timeline_events WHEN NEW.session_id = 'bad' BEGIN SELECT RAISE(ABORT, 'injected failure'); END").unwrap();
    // The writer is held after a round of `warm`, so the three writes below
    // meet in its next round.
    let release = hold_writer(&store, key("warm", "main"), vec![text(1, "warm")]).await;
    let bad_event = record(
        2,
        EventKind::BranchCreate(BranchCreateData {
            at: BranchCreateAt::Create {},
            source: BranchSource::Host {
                operation_id: "fork".into(),
            },
            branch: "missing".into(),
            prompt: String::new(),
        }),
    );
    let bad = queue(&store, key("bad", "main"), vec![bad_event]);
    let good = queue(&store, key("good", "main"), vec![text(1, "good")]);
    let other = queue(&store, key("other", "main"), vec![text(1, "other")]);
    drop(release);
    assert!(matches!(bad.await, Err(AgentError::Storage(_))));
    good.await.unwrap();
    other.await.unwrap();
    assert_eq!(store.writer_memo_requests("bad"), 0);
    assert_eq!(store.head(&key("bad", "main")).await.unwrap(), 1);
    assert_eq!(store.branches("bad").await.unwrap().len(), 1);
    assert!(
        store
            .append(
                key("bad", "main"),
                vec![text(2, "late")],
                Box::new(|_| panic!("rejected append callback must not run"))
            )
            .is_err()
    );
    assert_eq!(store.head(&key("good", "main")).await.unwrap(), 1);
    assert_eq!(store.head(&key("other", "main")).await.unwrap(), 1);
    connection.execute_batch("DROP TRIGGER fail_bad").unwrap();
    drop(connection);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_output_commit_closes_only_its_session_and_recovery_settles_the_orphan() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        builder.timeline_store(store.clone());
        let agent = builder.build();
        let bad = agent.create_session(SessionConfig::default()).await.unwrap();
        let good = agent.create_session(SessionConfig::default()).await.unwrap();
        let bad_id = bad.id().to_owned();
        let connection = Connection::open(&database.0).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_output BEFORE INSERT ON timeline_events WHEN NEW.kind = 'step/text' AND NEW.session_id = (SELECT session_id FROM injected_session) BEGIN SELECT RAISE(ABORT, 'injected output commit failure'); END; CREATE TABLE injected_session(session_id TEXT)").unwrap();
        connection.execute("INSERT INTO injected_session VALUES (?1)", [&bad_id]).unwrap();
        let (admitted_bad, admitted_good) = tokio::join!(
            bad.submit("failed output".into(), InputMode::Queue, "main"),
            good.submit("healthy output".into(), InputMode::Queue, "main"),
        );
        admitted_bad.unwrap();
        admitted_good.unwrap();
        let error = bad.closed().await.unwrap_err();
        assert!(error.to_string().contains("injected output commit failure"));
        good.idle("main").await.unwrap();
        // The writer remembers the last request a running Session wrote and
        // forgets a Session whose commit failed.
        assert_eq!(store.writer_memo_requests(good.id()), 1);
        assert_eq!(store.writer_memo_requests(&bad_id), 0);
        assert!(agent.get_session(&bad_id).is_none());
        assert!(agent.get_session(good.id()).is_some());
        let bad_history = bad.history("main", None).await.unwrap();
        assert!(bad_history.iter().any(|event| matches!(event.kind, EventKind::ModelCall(_))));
        assert!(!bad_history.iter().any(|event| matches!(event.kind, EventKind::Text(_) | EventKind::ModelOutputEnd(_) | EventKind::TurnEnd(_))));
        assert!(bad.submit("late".into(), InputMode::Queue, "main").await.unwrap_err().to_string().contains("injected output commit failure"));
        assert!(good.history("main", None).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("healthy output"))));
        connection.execute_batch("DROP TRIGGER fail_output; DROP TABLE injected_session").unwrap();
        drop(connection);
        let _ = agent.close().await;
        drop(bad);
        drop(good);
        drop(agent);
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(builder);
        drop(store);
        drop(db);

        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        builder.timeline_store(store.clone());
        let restored = builder.build();
        let session = restored.restore_session(bad_id).await.unwrap();
        session.idle("main").await.unwrap();
        let history = session.history("main", None).await.unwrap();
        assert!(history.iter().any(|event| matches!(&event.kind, EventKind::ModelOutputEnd(data) if matches!(data.outcome, ModelOutputOutcome::Failed { .. }))));
        assert!(history.iter().any(|event| matches!(&event.kind, EventKind::TurnEnd(data) if matches!(data.outcome, TurnOutcome::Failed { .. }))));
        session.submit("after recovery".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        assert!(session.history("main", None).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("after recovery"))));
        // Once the Session unloads, the writer memo holds no live request of it.
        assert_eq!(store.writer_memo_requests(session.id()), 1);
        assert!(session.unload_if_idle().await.unwrap());
        session.closed().await.unwrap();
        let id = session.id().to_owned();
        drop(session);
        assert_eq!(store.writer_memo_requests(&id), 0);
        restored.close().await.unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
    }).await.expect("Session failure and recovery completed");
}

#[tokio::test]
async fn recovery_rejects_corrupt_records_and_preserves_them() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    append(&store, key("session", "main"), vec![text(1, "saved")])
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let connection = Connection::open(&database.0).unwrap();
    let original: String = connection
        .query_row("SELECT body FROM timeline_events", [], |row| row.get(0))
        .unwrap();
    let mut corrupt: serde_json::Value = serde_json::from_str(&original).unwrap();
    corrupt["records"][0]["batch_end"] = 2.into();
    let corrupt = serde_json::to_string(&corrupt).unwrap();
    connection
        .execute("UPDATE timeline_events SET body = ?1", [&corrupt])
        .unwrap();
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    assert!(matches!(
        store.branches("session").await,
        Err(AgentError::Storage(_))
    ));
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT body FROM timeline_events", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        corrupt
    );
    drop(store);
    drop(db);
    connection
        .execute_batch("PRAGMA foreign_keys = OFF")
        .unwrap();
    connection
        .execute(
            "UPDATE timeline_events SET body = ?1, branch_id = 'missing'",
            [&original],
        )
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    assert!(matches!(
        store.branches("session").await,
        Err(AgentError::Storage(_))
    ));
    store.close().await.unwrap();
    db.close().await.unwrap();
    assert_eq!(
        connection
            .query_row("SELECT branch_id FROM timeline_events", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "missing"
    );

    // Damaged request lists: each damage fails every read of the request
    // and the recovery validation with a storage error, and the row keeps
    // its bytes. Sequence 2 calls the request 1 prepared, so a damaged list
    // there is also checked against the request read before it.
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let request = |messages: Vec<Message>| {
        let mut request = ModelRequest::new("mock", "test", messages);
        request.system = Some(Arc::new(SystemPrompt::Text("system".into())));
        request.tools = Some(Arc::new(vec![serde_json::from_value(serde_json::json!({"name":"echo", "description":"echo", "inputSchema":{"type":"object"}, "source":"static"})).unwrap()]));
        Arc::new(request)
    };
    let prepare = |sequence, request: &Arc<ModelRequest>| {
        record(
            sequence,
            EventKind::ContextPrepare(ContextPrepareData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                through: 0,
                request: request.clone(),
                configuration: None,
                prompt: None,
                recovery: None,
            }),
        )
    };
    let call = |sequence, request: &Arc<ModelRequest>| {
        record(
            sequence,
            EventKind::ModelCall(ModelCallData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                request: request.clone(),
                configuration: None,
                prompt: None,
            }),
        )
    };
    let messages = |prefix: &str, count: usize| -> Vec<Message> {
        (0..count)
            .map(|index| Message::text(Role::User, format!("{prefix} {index}")))
            .collect()
    };
    let short = request(messages("m", 3));
    // A list long enough to cross holds of node loads, a compaction, and a
    // new list after it.
    let long = request(messages("long", 600));
    let after = request(messages("after", 2));
    for (session, records) in [
        ("lists", vec![prepare(1, &short), call(2, &short)]),
        (
            "long",
            vec![
                prepare(1, &long),
                call(2, &long),
                record(
                    3,
                    EventKind::ContextCompaction(completed("event-1", "summary", 2)),
                ),
                prepare(4, &after),
                call(5, &after),
            ],
        ),
    ] {
        store
            .create_session(session, &SessionConfig::default())
            .await
            .unwrap();
        for record in records {
            append(&store, key(session, "main"), vec![record])
                .await
                .unwrap();
        }
    }
    let connection = Connection::open(&database.0).unwrap();
    let hash = |session: &str, kind: &str, like: &str| -> String {
        connection
            .query_row(
                "SELECT hash FROM timeline_content WHERE session_id = ?1 AND kind = ?2 AND body LIKE ?3",
                [session, kind, like],
                |row| row.get(0),
            )
            .unwrap()
    };
    let event: String = connection
        .query_row(
            "SELECT body FROM timeline_events WHERE session_id = 'lists' AND sequence = 2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let (last, system, tools) = (
        hash("lists", "node", "%m 2%"),
        hash("lists", "system", "%"),
        hash("lists", "tools", "%"),
    );
    let middle = hash("lists", "node", "%m 1%");
    let node: String = connection
        .query_row(
            "SELECT body FROM timeline_content WHERE hash = ?1",
            [&middle],
            |row| row.get(0),
        )
        .unwrap();
    let refs =
        format!(r#""refs":{{"list":"{last}","length":3,"tools":"{tools}","system":"{system}"}}"#);
    assert!(event.contains(&refs));
    let unparseable = "not a node";
    let unparseable_hash = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(format!("node\0{unparseable}"))
    );
    connection
        .execute(
            "INSERT INTO timeline_content VALUES ('lists', ?1, 'node', ?2)",
            [&unparseable_hash, unparseable],
        )
        .unwrap();
    let damaged_refs = |list: &str, length: &str, tools: &str, system: &str| {
        event.replace(
            &refs,
            &format!(r#""refs":{{"list":{list},"length":{length},"tools":"{tools}","system":"{system}"}}"#),
        )
    };
    let quoted = |hash: &str| format!("\"{hash}\"");
    for (table, intact, damaged, failure) in [
        (
            "timeline_events",
            event.clone(),
            damaged_refs(&quoted(&last.to_uppercase()), "3", &tools, &system),
            "missing request content",
        ),
        (
            "timeline_content",
            node.clone(),
            node.replace("m 1", "m one"),
            "request content hash mismatch",
        ),
        (
            "timeline_events",
            event.clone(),
            damaged_refs(&quoted(&system), "3", &tools, &system),
            "request list references wrong content kind",
        ),
        (
            "timeline_events",
            event.clone(),
            damaged_refs(&quoted(&unparseable_hash), "1", &tools, &system),
            "expected",
        ),
        (
            "timeline_events",
            event.clone(),
            damaged_refs(&quoted(&last), "2", &tools, &system),
            "request list length mismatch",
        ),
        (
            "timeline_events",
            event.clone(),
            damaged_refs(&quoted(&last), "4", &tools, &system),
            "request list length mismatch",
        ),
        (
            "timeline_events",
            event.clone(),
            damaged_refs("null", "3", &tools, &system),
            "request list length mismatch",
        ),
        (
            "timeline_events",
            event.clone(),
            event.replace(r#","length":3"#, ""),
            "request references are neither messages nor a list",
        ),
        (
            "timeline_events",
            event.clone(),
            event.replace(r#","length":3"#, r#","length":3,"messages":[]"#),
            "request references are neither messages nor a list",
        ),
        (
            "timeline_events",
            event.clone(),
            event.replace(r#","length":3"#, r#","length":3,"next":null"#),
            "unknown field",
        ),
        (
            "timeline_events",
            event.clone(),
            damaged_refs(&quoted(&last), "3", &system, &system),
            "request tools reference wrong content kind",
        ),
        (
            "timeline_events",
            event.clone(),
            damaged_refs(&quoted(&last), "3", &tools, &tools),
            "request system references wrong content kind",
        ),
    ] {
        assert_ne!(damaged, intact);
        let set = format!("UPDATE {table} SET body = ?1 WHERE session_id = 'lists' AND body = ?2");
        assert_eq!(connection.execute(&set, [&damaged, &intact]).unwrap(), 1);
        assert!(
            matches!(
                store.read(&key("lists", "main"), 1..=2).await,
                Err(AgentError::Storage(message)) if message.contains(failure)
            ),
            "{failure}"
        );
        assert!(matches!(
            store.read_page(&key("lists", "main"), 0, 2, 2).await,
            Err(AgentError::Storage(message)) if message.contains(failure)
        ));
        assert!(matches!(
            store.branches("lists").await,
            Err(AgentError::Storage(message)) if message.contains(failure)
        ));
        // The bad row kept its bytes.
        assert_eq!(connection.execute(&set, [&intact, &damaged]).unwrap(), 1);
    }
    store.branches("lists").await.unwrap();

    // A damaged node in the middle of a long list fails every read whose
    // requests pass through it, and none whose requests come after the
    // compaction.
    let middle = hash("long", "node", "%long 300%");
    let node: String = connection
        .query_row(
            "SELECT body FROM timeline_content WHERE hash = ?1",
            [&middle],
            |row| row.get(0),
        )
        .unwrap();
    let set = "UPDATE timeline_content SET body = ?1 WHERE session_id = 'long' AND body = ?2";
    let damaged = node.replace("long 300", "long three hundred");
    assert_eq!(connection.execute(set, [&damaged, &node]).unwrap(), 1);
    for range in [1..=1, 2..=2, 1..=5] {
        assert!(matches!(
            store.read(&key("long", "main"), range).await,
            Err(AgentError::Storage(message)) if message.contains("request content hash mismatch")
        ));
    }
    assert!(matches!(
        store.branches("long").await,
        Err(AgentError::Storage(message)) if message.contains("request content hash mismatch")
    ));
    let later = store.read(&key("long", "main"), 3..=5).await.unwrap();
    assert_eq!(later[2].event.kind, call(5, &after).event.kind);
    assert_eq!(
        store
            .read_page(&key("long", "main"), 3, 2, 5)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(connection.execute(set, [&node, &damaged]).unwrap(), 1);
    store.branches("long").await.unwrap();
    drop(connection);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn invalid_ancestor_directory_rejects_only_its_session() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    for session in ["broken", "healthy"] {
        store
            .create_session(session, &SessionConfig::default())
            .await
            .unwrap();
    }
    append(
        &store,
        key("broken", "main"),
        vec![record(
            1,
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: BranchSource::Host {
                    operation_id: "first".into(),
                },
                branch: "z".into(),
                prompt: "first".into(),
            }),
        )],
    )
    .await
    .unwrap();
    let mut nested = record(
        1,
        EventKind::BranchCreate(BranchCreateData {
            at: BranchCreateAt::Create {},
            source: BranchSource::Host {
                operation_id: "second".into(),
            },
            branch: "a".into(),
            prompt: "second".into(),
        }),
    );
    Arc::make_mut(&mut nested.event).id = "nested-create".into();
    append(&store, key("broken", "z"), vec![nested])
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);

    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute(
            "UPDATE timeline_branches SET info = json_set(info, '$.parent', 'missing') WHERE session_id = 'broken' AND branch_id = 'z'",
            [],
        )
        .unwrap();
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    assert!(matches!(
        store.branches("broken").await,
        Err(AgentError::Storage(_))
    ));
    assert_eq!(store.branches("healthy").await.unwrap().len(), 1);
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    let info: String = connection
        .query_row(
            "SELECT info FROM timeline_branches WHERE session_id = 'broken' AND branch_id = 'z'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&info).unwrap()["parent"],
        "missing"
    );
}

#[test]
fn unsupported_legacy_or_changed_schema_is_rejected_without_migration() {
    /// The store a case changes.
    enum Base {
        Empty,
        V13,
        V15,
        V16,
        V17,
        V18,
    }
    for (base, sql) in [
        (Base::Empty, "CREATE TABLE sessions (id TEXT PRIMARY KEY)"),
        (
            Base::Empty,
            "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 1)); INSERT INTO agent_store_format VALUES (1)",
        ),
        (
            Base::Empty,
            "CREATE TABLE agent_store_format (version INTEGER PRIMARY KEY); INSERT INTO agent_store_format VALUES (99)",
        ),
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 1));
            INSERT INTO agent_store_format VALUES (1);
            INSERT INTO agents VALUES ('legacy', '{"name":"legacy","description":"","systemPrompt":"","tools":[]}', 1)"#,
        ),
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 2));
            INSERT INTO agent_store_format VALUES (2);"#,
        ),
        // v4 records keep input state, business blocks and prompt deliveredBy.
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 4));
            INSERT INTO agent_store_format VALUES (4);"#,
        ),
        // v5 calls closed only by projection fill-ins have no closing fact (agent-029).
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 5));
            INSERT INTO agent_store_format VALUES (5);"#,
        ),
        // v6 stores a derived account catalog, without canonical model overrides.
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 6));
            INSERT INTO agent_store_format VALUES (6);"#,
        ),
        // v7 selections identify only a model; v8 fixes the full provider/model identity.
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 7));
            INSERT INTO agent_store_format VALUES (7);"#,
        ),
        // v8 has no managed OAuth binding in the provider record.
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 8));
            INSERT INTO agent_store_format VALUES (8);"#,
        ),
        // v10 has a private account catalog JSON variant, not normalized managed records.
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 10));
            INSERT INTO agent_store_format VALUES (10);"#,
        ),
        // v11's inline checkpoint schema must be preserved and rejected.
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 11));
            INSERT INTO agent_store_format VALUES (11);
            DROP TABLE timeline_snapshots;
            ALTER TABLE timeline_events ADD COLUMN checkpoint INTEGER NOT NULL DEFAULT 0;
            CREATE INDEX timeline_events_checkpoints ON timeline_events(session_id,branch_id,checkpoint,sequence);"#,
        ),
        // v9 is an unpublished development format without import markers.
        (
            Base::V13,
            r#"DROP TABLE provider_imports; DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 9));
            INSERT INTO agent_store_format VALUES (9);
            INSERT INTO providers VALUES ('preserve',7,'{"credential":"original"}');"#,
        ),
        // The removed branch-only v14 envelope must not be downgraded to v13.
        (
            Base::V13,
            r#"DROP TABLE settings_documents; DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 14));
            INSERT INTO agent_store_format VALUES (14);
            CREATE TABLE user_configuration (layer TEXT NOT NULL PRIMARY KEY CHECK(layer = 'user'), version INTEGER NOT NULL CHECK(version > 0), envelope TEXT NOT NULL);
            INSERT INTO user_configuration VALUES ('user', 7, '{"format":1,"patches":[],"documents":{},"imports":{}}');"#,
        ),
        // A matching v13 schema cannot disguise a v14 format marker either.
        (
            Base::V13,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=14",
        ),
        // A matching schema still requires a valid format marker; a v13
        // schema without marker 13 is never upgraded.
        (Base::V13, "DELETE FROM agent_store_format"),
        (
            Base::V13,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=99",
        ),
        // A v15 schema requires marker 15; it is never read as v13 or v16.
        (Base::V15, "DELETE FROM agent_store_format"),
        (
            Base::V15,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=13",
        ),
        (
            Base::V15,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=16",
        ),
        // A v16 schema requires marker 16; it is never read as v15 or v17.
        (Base::V16, "DELETE FROM agent_store_format"),
        (
            Base::V16,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=15",
        ),
        (
            Base::V16,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=17",
        ),
        // A v17 schema requires marker 17; it is never read as v16 or v18.
        (Base::V17, "DELETE FROM agent_store_format"),
        (
            Base::V17,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=16",
        ),
        (
            Base::V17,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=18",
        ),
        // A v18 schema requires marker 18; it is never read as v17.
        (Base::V18, "DELETE FROM agent_store_format"),
        (
            Base::V18,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=17",
        ),
        // A partly upgraded store is neither format.
        (
            Base::V13,
            "CREATE TABLE session_files (file_id TEXT NOT NULL PRIMARY KEY, node TEXT, writer TEXT)",
        ),
        (Base::V15, "DROP INDEX session_file_owners_by_file"),
        (Base::V15, V16_ADDITIONS[0]),
        (Base::V16, "DROP TABLE session_metadata"),
        (Base::V17, "DROP TABLE session_metadata"),
        (Base::V18, "DROP TABLE session_metadata"),
    ] {
        let database = Database::new();
        match base {
            Base::Empty => {}
            // v1 to v13 used the same tables; only their markers differ.
            Base::V13 => drop(v13_store(&database.0)),
            Base::V15 => drop(v15_store(&database.0)),
            Base::V16 => drop(v16_tables_store(&database.0, 16)),
            Base::V17 => drop(v16_tables_store(&database.0, 17)),
            Base::V18 => drop(SqliteStorage::open(database.0.clone()).unwrap()),
        }
        let connection = Connection::open(&database.0).unwrap();
        connection.execute_batch(sql).unwrap();
        drop(connection);
        let original = std::fs::read(&database.0).unwrap();
        for _ in 0..2 {
            // A failed startup must release its maintenance lease for retry.
            let Err(AgentError::Storage(message)) = SqliteStorage::open(database.0.clone()) else {
                panic!("unsupported database must return a storage error");
            };
            assert!(message.starts_with("unsupported "), "{message}");
            assert!(
                message.contains(&database.0.canonicalize().unwrap().display().to_string()),
                "{message}"
            );
            assert!(message.contains("database was preserved"), "{message}");
            assert!(message.contains("compatible daemon"), "{message}");
            assert!(message.contains("new database path"), "{message}");
            assert_eq!(std::fs::read(&database.0).unwrap(), original);
        }
    }
}

fn marker(connection: &Connection) -> Vec<i64> {
    connection
        .prepare("SELECT version FROM agent_store_format")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Upgrades the disposable `connection`'s store, built at `from` and holding
/// every documented row of the tables it has, to the exact v18 store in place:
/// every business row is unchanged, the added tables are empty, and only that
/// open reports the upgrade. The documented v16 request row then reads and
/// continues as mixed rows.
async fn upgrades_in_place_once(database: &Database, connection: Connection, from: u32) {
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();
    let added: Vec<&str> = V16_TABLES
        .iter()
        .filter(|_| from < 16)
        .chain(V15_TABLES.iter().filter(|_| from < 15))
        .copied()
        .collect();
    let kept: Vec<&str> = BUSINESS_TABLES
        .iter()
        .filter(|table| !added.contains(table))
        .copied()
        .collect();
    insert_documented_records(&connection, &kept);
    let before: Vec<_> = kept.iter().map(|table| rows(&connection, table)).collect();
    assert!(before.iter().filter(|rows| !rows.is_empty()).count() >= 7);
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(from));
    let store = SqliteTimelineStore::new(db.clone());
    assert_eq!(store.branches("s1").await.unwrap().len(), 2);
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(18));
    assert_eq!(marker(&connection), [18]);
    let after: Vec<_> = kept.iter().map(|table| rows(&connection, table)).collect();
    assert_eq!(after, before);
    for table in added {
        assert!(rows(&connection, table).is_empty());
    }

    // The upgraded store reopens as v18 and reports nothing. `main`'s next
    // request row is a v17 list; its previous request, the v16 row at 4, has no
    // list to share, so it writes a node for every message, and content
    // addresses meet the node the child already has for "Hi".
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    let store = SqliteTimelineStore::new(db.clone());
    let mut request = ModelRequest::new(
        "mock",
        "deterministic",
        vec![
            Message::text(Role::User, "Hi"),
            Message::text(Role::Assistant, "Hello there"),
            Message::text(Role::User, "Again"),
        ],
    );
    request.system = Some(Arc::new(SystemPrompt::Text("You are helpful.".into())));
    let continued = record(
        9,
        EventKind::ModelCall(ModelCallData {
            turn_id: "t3".into(),
            request_id: "r3".into(),
            request: Arc::new(request),
            configuration: None,
            prompt: None,
        }),
    );
    let content = rows(&connection, "timeline_content").len();
    append(&store, key("s1", "main"), vec![continued.clone()])
        .await
        .unwrap();
    let main = store.read(&key("s1", "main"), 1..=9).await.unwrap();
    assert_eq!(main[8].event, continued.event);
    let EventKind::ModelCall(legacy) = &main[3].event.kind else {
        panic!("the v16 row reads back")
    };
    assert_eq!(
        legacy.request.messages,
        [Message::text(Role::User, "Hi")].map(Arc::new)
    );
    assert_eq!(rows(&connection, "timeline_content").len(), content + 2);
    let refs: serde_json::Value = connection
        .query_row(
            "SELECT json_extract(body, '$.record.refs') FROM timeline_events WHERE session_id = 's1' AND branch_id = 'main' AND sequence = 9",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|refs| serde_json::from_str(&refs).unwrap())
        .unwrap();
    assert_eq!(refs["length"], 3);
    assert_eq!(
        refs["system"],
        "eed2128b01236e0436f9b7ae565767c078e5e6f1d13dcacd51fa8399564c6288"
    );
    assert_eq!(store.branches("s1").await.unwrap().len(), 2);
    drop(connection);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// An empty store is created at exactly v18; a v13 store takes S5a's,
/// S5b's, #193's and #2's one-way steps in one open.
#[tokio::test]
async fn a_v13_store_upgrades_in_place_once() {
    let fresh = Database::new();
    let db = SqliteStorage::open(fresh.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    db.close().await.unwrap();
    let connection = Connection::open(&fresh.0).unwrap();
    assert_eq!(schema(&connection), schema_of(18));
    assert_eq!(marker(&connection), [18]);
    drop(connection);

    let database = Database::new();
    let connection = v13_store(&database.0);
    upgrades_in_place_once(&database, connection, 13).await;
}

/// S5b's one-way upgrade, then #193's and #2's: an exact v15 store becomes
/// the exact v18 store.
#[tokio::test]
async fn a_v15_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v15_store(&database.0);
    assert_eq!(schema(&connection), schema_of(15));
    upgrades_in_place_once(&database, connection, 15).await;
}

/// #193's and #2's one-way upgrades replace only the marker of an exact v16
/// store.
#[tokio::test]
async fn a_v16_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 16);
    assert_eq!(schema(&connection), schema_of(16));
    upgrades_in_place_once(&database, connection, 16).await;
}

/// #2's one-way upgrade replaces only the marker of an exact v17 store: its
/// rows, v16 and v17 request rows alike, are unchanged.
#[tokio::test]
async fn a_v17_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 17);
    assert_eq!(schema(&connection), schema_of(17));
    upgrades_in_place_once(&database, connection, 17).await;
}

/// The daemon names the one-way upgrade on stderr, where its default log
/// filter would hide a tracing event, and only on the start that upgrades.
#[cfg(unix)]
#[tokio::test]
async fn the_daemon_reports_a_v13_upgrade_on_stderr_once() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let database = Database::new();
    drop(v13_store(&database.0));
    let path = database.0.canonicalize().unwrap();
    let socket = PathBuf::from("/tmp").join(format!("dim-upgrade-{}/s", uuid::Uuid::new_v4()));
    let mut reports = vec![];
    for _ in 0..2 {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
            .args(["--mock-model", "--no-ws", "--unix"])
            .arg(&socket)
            .arg("--database")
            .arg(&database.0)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut ready = String::new();
        tokio::time::timeout(
            Duration::from_secs(10),
            BufReader::new(child.stdout.take().unwrap()).read_line(&mut ready),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(ready.contains("unixPath"), "{ready}");
        child.kill().await.unwrap();
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .await
            .unwrap();
        reports.push(stderr);
    }
    let line = format!(
        "Upgraded native store {} from format v13 to v18; earlier dimcode versions cannot open it.",
        path.display()
    );
    assert!(
        reports[0].lines().any(|report| report == line),
        "{reports:?}"
    );
    assert!(!reports[1].contains("Upgraded native store"), "{reports:?}");
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
}

#[tokio::test]
async fn owned_session_creation_and_plugin_shutdown_keep_one_resource_owner() {
    let database = Database::new();
    let root = rutis::Ctx::root().unwrap();
    let sqlite = root.plugin(capability_plugin(
        dim_app_server::storage::SqliteStoragePlugin::new(database.0.clone()),
    ));
    let session = root.plugin(capability_plugin(
        dim_app_server::storage::SessionStorePlugin::new(0),
    ));
    (&sqlite).await.unwrap();
    (&session).await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute(
            "INSERT INTO agents VALUES ('owner', ?1, 1)",
            [serde_json::to_string(&AgentProfile {
                name: "owner".into(),
                description: String::new(),
                system_prompt: String::new(),
                tools: vec![],
                model: None,
            })
            .unwrap()],
        )
        .unwrap();
    let directory = root.get::<dim_app_server::storage::SessionStore>().unwrap();
    let retained_db = root.get::<SqliteStorage>().unwrap();
    assert!(
        directory
            .create_owned_session("missing-owner", &SessionConfig::default(), "unknown", 2)
            .await
            .is_err()
    );
    assert!(directory.sessions().await.unwrap().is_empty());
    directory
        .create_owned_session("owned", &SessionConfig::default(), "owner", 2)
        .await
        .unwrap();
    assert_eq!(directory.sessions().await.unwrap(), ["owned"]);
    assert_eq!(
        connection
            .query_row(
                "SELECT agent_id FROM session_owners WHERE session_id = ?1",
                params!["owned"],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "owner"
    );
    let store = directory.timeline_store();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    store
        .append(
            key("owned", "main"),
            vec![text(1, "shutdown pending")],
            Box::new(move |result| {
                let _ = sender.send(result);
            }),
        )
        .unwrap();
    root.shutdown().await.unwrap();
    receiver.await.unwrap().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT sequence FROM timeline_branches WHERE session_id = 'owned'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(matches!(
        directory.sessions().await,
        Err(AgentError::Storage(_))
    ));
    drop(connection);
    let replacement = SqliteStorage::open(database.0.clone()).unwrap();
    retained_db.close().await.unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    replacement.close().await.unwrap();

    let plugin = dim_agent_runtime::capability_plugin(
        dim_app_server::storage::SqliteStoragePlugin::new(database.0.clone()),
    );
    assert!(rutis::Plugin::apply(&plugin, &root).await.is_err());
    let reopened = SqliteStorage::open(database.0.clone()).unwrap();
    reopened.close().await.unwrap();
}

// Hold a child before exec, when close-on-exec descriptors are still inherited.
// The database must release ownership on close, not when that child reaches exec.
#[cfg(unix)]
#[tokio::test]
async fn sqlite_close_releases_ownership_while_a_spawned_child_inherits_descriptors() {
    use std::{
        io::{Read, Write},
        os::{fd::AsRawFd, unix::net::UnixStream, unix::process::CommandExt},
    };
    unsafe extern "C" {
        fn read(fd: i32, buffer: *mut u8, count: usize) -> isize;
        fn write(fd: i32, buffer: *const u8, count: usize) -> isize;
    }

    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let (mut parent, child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let spawning = std::thread::spawn(move || {
        let fd = child.as_raw_fd();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.arg("--list").stdout(std::process::Stdio::null());
        // SAFETY: the child uses only async-signal-safe read/write before exec;
        // its socket and both one-byte buffers remain live throughout the calls.
        unsafe {
            command.pre_exec(move || {
                let mut byte = 1_u8;
                if write(fd, &byte, 1) != 1 || read(fd, &mut byte, 1) != 1 {
                    return Err(std::io::Error::from_raw_os_error(5));
                }
                Ok(())
            });
        }
        command.spawn().unwrap().wait().unwrap()
    });
    let mut ready = [0];
    parent.read_exact(&mut ready).unwrap();
    assert_eq!(ready, [1]);
    let closed = db.close().await;
    let replacement = SqliteStorage::open(database.0.clone());
    // Always release and reap the child before asserting the observed result.
    parent.write_all(&[1]).unwrap();
    assert!(spawning.join().unwrap().success());
    closed.unwrap();
    let replacement = replacement.expect("successful close must release native ownership");
    db.close().await.unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    replacement.close().await.unwrap();
}

#[test]
fn sqlite_shutdown_drains_cancelled_and_queued_operations_before_releasing_ownership() {
    use std::{future::Future, task::Poll};
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let config = SessionConfig::default();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.await.unwrap();

        let mut accepted = store.create_session("accepted", &config);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(accepted.as_mut().poll(cx).is_pending())).await
        );
        let mut cancelled = store.create_session("cancelled", &config);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_pending())).await
        );
        drop(cancelled);
        let mut cancelled_close = Box::pin(db.close());
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(cancelled_close.as_mut().poll(cx).is_pending()))
                .await
        );
        drop(cancelled_close);
        let mut late = store.create_session("late", &config);
        assert!(matches!(
            std::future::poll_fn(|cx| Poll::Ready(late.as_mut().poll(cx))).await,
            Poll::Ready(Err(AgentError::Storage(_)))
        ));
        assert!(SqliteStorage::open(database.0.clone()).is_err());
        release.send(()).unwrap();
        blocker.await.unwrap();
        accepted.await.unwrap();
        store.close().await.unwrap();

        let replacement = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(reopened) = SqliteStorage::open(database.0.clone()) {
                    break reopened;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("cancelled shutdown must finish without another close caller");
        let replacement_store = SqliteTimelineStore::new(replacement.clone());
        for session in ["accepted", "cancelled"] {
            assert!(
                replacement_store
                    .session_config(session)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            replacement_store
                .session_config("late")
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            store.create_session("stale", &config).await,
            Err(AgentError::Storage(_))
        ));
        db.close().await.unwrap();
        assert!(SqliteStorage::open(database.0.clone()).is_err());
        replacement_store.close().await.unwrap();
        replacement.close().await.unwrap();
    });
}

#[tokio::test]
async fn concurrent_and_cancelled_close_waits_for_the_same_durable_round() {
    use std::{future::Future, task::Poll};
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    append(&store, key("session", "main"), vec![text(1, "warm")])
        .await
        .unwrap();
    let mut connection = Connection::open(&database.0).unwrap();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    store
        .append(
            key("session", "main"),
            vec![text(2, "pending")],
            Box::new(move |result| {
                let _ = sender.send(result);
            }),
        )
        .unwrap();
    let mut cancelled = Box::pin(store.close());
    let completed =
        std::future::poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await;
    assert!(!completed);
    drop(cancelled);
    let mut close = Box::pin(store.close());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), close.as_mut())
            .await
            .is_err()
    );
    transaction.commit().unwrap();
    close.await.unwrap();
    receiver.await.unwrap().unwrap();
    assert_eq!(store.head(&key("session", "main")).await.unwrap(), 2);
    db.close().await.unwrap();
}

#[tokio::test]
async fn damaged_snapshot_cannot_silently_remove_retained_output_after_restart() {
    use dim_agent_capabilities::history::{
        context_messages, project_context_history, projection_checkpoint, summary_message,
    };

    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    append(
        &store,
        key("session", "main"),
        vec![
            record(
                1,
                EventKind::InputMessage(InputMessageData::text("question", None)),
            ),
            record(
                2,
                EventKind::InputConsumed(InputConsumedData {
                    input_id: "event-1".into(),
                }),
            ),
            record(
                3,
                EventKind::ModelStart(ModelStartData {
                    turn_id: "turn".into(),
                    request_id: "request".into(),
                }),
            ),
            text(4, "RETAINED ANSWER"),
            record(
                5,
                EventKind::ModelOutputEnd(ModelOutputEndData {
                    turn_id: "turn".into(),
                    request_id: "request".into(),
                    outcome: ModelOutputOutcome::Completed {
                        finish_reason: FinishReason::EndTurn,
                    },
                }),
            ),
        ],
    )
    .await
    .unwrap();
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    // The cut keeps the answer: the model-start of its request.
    let checkpoint = projection_checkpoint(&timeline.context(5).await.unwrap(), "event-3").unwrap();
    assert_eq!(checkpoint.resume.sequence, 5);
    assert!(!checkpoint.state.output.is_empty());
    let compaction = completed("event-3", "summary", 5);
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    append(
        &store,
        key("session", "main"),
        vec![record(6, EventKind::ContextCompaction(compaction))],
    )
    .await
    .unwrap();
    let saved = snapshot(6, &checkpoint);
    store
        .save_snapshot(&key("session", "main"), saved.clone())
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let connection = Connection::open(&database.0).unwrap();
    let facts = || {
        connection
            .prepare("SELECT body FROM timeline_events ORDER BY sequence")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let original = facts();
    let mut damaged: serde_json::Value = serde_json::from_str(&saved.state).unwrap();
    let mut payload: serde_json::Value =
        serde_json::from_str(damaged["payload"].as_str().unwrap()).unwrap();
    payload["state"]["output"] = serde_json::json!([]);
    damaged["payload"] = payload.to_string().into();
    for (state, intact) in [(saved.state, true), (damaged.to_string(), false)] {
        connection
            .execute("UPDATE timeline_snapshots SET state=?1", [state])
            .unwrap();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store.branches("session").await.unwrap();
        let timeline = Timeline::restore(
            rutis::Ctx::root().unwrap(),
            key("session", "main"),
            vec![],
            store.clone(),
        )
        .await
        .unwrap();
        let full = timeline.context(6).await.unwrap();
        let recovered = read_context(&timeline, 6).await.unwrap();
        let expected = vec![
            summary_message("summary"),
            Message {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![MessagePart::Text {
                    provider_metadata: None,
                    text: "RETAINED ANSWER".into(),
                    start_time: None,
                    end_time: None,
                }]),
            },
        ];
        assert_eq!(
            context_messages(&full, &project_context_history(&full).unwrap()),
            expected
        );
        assert_eq!(
            context_messages(&recovered, &project_context_history(&recovered).unwrap()),
            expected
        );
        assert_eq!(recovered.checkpoint.is_some(), intact);
        dim_agent_runtime::core::close_timeline(&timeline)
            .await
            .unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
        assert_eq!(facts(), original);
    }
}

#[tokio::test]
async fn recovery_ignores_disposable_snapshots_but_preserves_and_validates_facts() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let mut input_batch = vec![
        record(
            1,
            EventKind::InputMessage(InputMessageData::text("history", Some(InputMode::Queue))),
        ),
        record(
            2,
            EventKind::InputConsumed(InputConsumedData {
                input_id: "event-1".into(),
            }),
        ),
    ];
    for event in &mut input_batch {
        event.batch_end = 2;
    }
    append(&store, key("session", "main"), input_batch)
        .await
        .unwrap();
    // Nothing is kept: the cut is the fact's own event, after every entry.
    let compaction = completed("event-3", "retained summary", 2);
    let original = record(3, EventKind::ContextCompaction(compaction));
    append(&store, key("session", "main"), vec![original.clone()])
        .await
        .unwrap();
    assert!(
        store
            .latest_snapshot(&key("session", "main"), 3, INTERPRETER)
            .await
            .unwrap()
            .is_none()
    );
    for (sequence, event_id) in [(3, "wrong"), (4, "event-4")] {
        assert!(
            store
                .save_snapshot(
                    &key("session", "main"),
                    RecoverySnapshot {
                        sequence,
                        event_id: event_id.into(),
                        interpreter: INTERPRETER.into(),
                        state: "opaque".into(),
                    }
                )
                .await
                .is_err()
        );
    }
    let opaque = RecoverySnapshot {
        sequence: 3,
        event_id: "event-3".into(),
        interpreter: INTERPRETER.into(),
        state: "not json".into(),
    };
    store
        .save_snapshot(&key("session", "main"), opaque.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .latest_snapshot(&key("session", "main"), 3, INTERPRETER)
            .await
            .unwrap(),
        Some(opaque)
    );
    assert!(
        store
            .latest_snapshot(&key("session", "main"), 2, INTERPRETER)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .latest_snapshot(&key("session", "main"), 3, "other")
            .await
            .unwrap()
            .is_none()
    );
    // Anchoring is structural (#130): any existing event with its id and
    // sequence anchors a snapshot; its owner decides whether it is usable.
    let input_anchored = RecoverySnapshot {
        sequence: 1,
        event_id: "event-1".into(),
        interpreter: INTERPRETER.into(),
        state: "opaque".into(),
    };
    store
        .save_snapshot(&key("session", "main"), input_anchored.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .latest_snapshot(&key("session", "main"), 2, INTERPRETER)
            .await
            .unwrap(),
        Some(input_anchored)
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let connection = Connection::open(&database.0).unwrap();
    let body: String = connection
        .query_row(
            "SELECT body FROM timeline_events WHERE sequence = 3",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut states = vec![
        None,
        Some(("unsupported/v99", "not json".to_owned())),
        Some((INTERPRETER, "not json".to_owned())),
    ];
    // Sequence 2 is a legal resume address, but this state omits the pending
    // input consumed there. Decoding and anchor checks alone cannot accept it.
    for (branch, sequence) in [("unknown", 1), ("main", 0), ("main", 3), ("main", 2)] {
        states.push(Some((
            INTERPRETER,
            snapshot(
                3,
                &ProjectionCheckpoint {
                    resume: TimelinePosition {
                        branch_id: branch.into(),
                        sequence,
                    },
                    state: ProjectionCheckpointState {
                        retain_from: "event-3".into(),
                        ..Default::default()
                    },
                },
            )
            .state,
        )));
    }
    for state in states {
        connection
            .execute("DELETE FROM timeline_snapshots", [])
            .unwrap();
        if let Some((interpreter, state)) = state {
            connection
                .execute(
                    "INSERT INTO timeline_snapshots VALUES ('session','main',3,?1,'event-3',?2)",
                    params![interpreter, state],
                )
                .unwrap();
        }
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store.branches("session").await.unwrap();
        let timeline = Timeline::restore(
            rutis::Ctx::root().unwrap(),
            key("session", "main"),
            vec![],
            store.clone(),
        )
        .await
        .unwrap();
        let full = timeline.context(3).await.unwrap();
        assert!(full.checkpoint.is_none());
        let accelerated = read_context(&timeline, 3).await.unwrap();
        assert!(accelerated.checkpoint.is_none());
        use dim_agent_capabilities::history::{
            context_messages, project_context_history, summary_message,
        };
        assert_eq!(
            context_messages(
                &accelerated,
                &project_context_history(&accelerated).unwrap()
            ),
            [summary_message("retained summary")]
        );
        assert_eq!(
            project_context_history(&accelerated).unwrap(),
            project_context_history(&full).unwrap()
        );
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        builder.timeline_store(store.clone());
        let agent = builder.build();
        agent.restore_session("session".into()).await.unwrap();
        agent.close().await.unwrap();
        dim_agent_runtime::core::close_timeline(&timeline)
            .await
            .unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT body FROM timeline_events WHERE sequence=3",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            body
        );
    }
    for through in [1, 3, 4] {
        let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
        value["record"]["event"]["data"]["through"] = through.into();
        let bad = serde_json::to_string(&value).unwrap();
        connection
            .execute(
                "UPDATE timeline_events SET body = ?1 WHERE sequence = 3",
                [&bad],
            )
            .unwrap();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        assert!(matches!(
            store.branches("session").await,
            Err(AgentError::Storage(_))
        ));
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        assert_eq!(
            connection
                .query_row(
                    "SELECT body FROM timeline_events WHERE sequence = 3",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            bad
        );
    }
    connection
        .execute(
            "UPDATE timeline_events SET body = ?1 WHERE sequence = 3",
            [&body],
        )
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store.branches("session").await.unwrap();
    assert_eq!(
        store.read(&key("session", "main"), 3..=3).await.unwrap()[0].event,
        original.event
    );
    connection
        .execute("DELETE FROM timeline_snapshots", [])
        .unwrap();
    // Inject malformed cache metadata through this disposable connection;
    // ordinary writes retain their foreign-key enforcement.
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    // An anchor of another kind is structurally valid (#130) and is left to
    // the owner's decode, so only identity mismatches are injected here.
    for (branch, sequence, event_id) in [("main", 3, "wrong"), ("orphan", 99, "missing")] {
        connection
            .execute(
                "INSERT INTO timeline_snapshots VALUES ('session',?1,?2,?3,?4,'not json')",
                params![branch, sequence, INTERPRETER, event_id],
            )
            .unwrap();
    }
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    assert!(
        store
            .latest_snapshot(&key("session", "main"), 99, INTERPRETER)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .latest_snapshot(&key("session", "orphan"), 99, INTERPRETER)
            .await
            .unwrap()
            .is_none()
    );
    store.branches("session").await.unwrap();
    store.rollback_session("session").await.unwrap();
    for table in [
        "timeline_snapshots",
        "timeline_events",
        "timeline_branches",
        "timeline_content",
        "agent_sessions",
    ] {
        assert_eq!(
            connection
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE session_id='session'"),
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn nested_branches_read_fixed_ancestor_checkpoints_after_their_parents_close() {
    use dim_agent_capabilities::history::{
        context_messages, project_context_history, projection_checkpoint,
    };
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("nested", &SessionConfig::default())
        .await
        .unwrap();
    for stored in [
        record(
            1,
            EventKind::InputMessage(InputMessageData::text(
                "earlier input",
                Some(InputMode::Queue),
            )),
        ),
        record(
            2,
            EventKind::InputConsumed(InputConsumedData {
                input_id: "event-1".into(),
            }),
        ),
        record(
            3,
            EventKind::InputMessage(InputMessageData::text(
                "queued across compaction",
                Some(InputMode::Queue),
            )),
        ),
    ] {
        append(&store, key("nested", "main"), vec![stored])
            .await
            .unwrap();
    }
    let prefix = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("nested", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    // The cut replaces the earlier input; the queued one is not placed yet.
    let checkpoint = projection_checkpoint(&prefix.context(3).await.unwrap(), "event-3").unwrap();
    append(
        &store,
        key("nested", "main"),
        vec![record(
            4,
            EventKind::ContextCompaction(completed("event-3", "earlier summary", 3)),
        )],
    )
    .await
    .unwrap();
    store
        .save_snapshot(&key("nested", "main"), snapshot(4, &checkpoint))
        .await
        .unwrap();
    for (sequence, branch, cut) in [(5, "early", 2), (6, "child", 4)] {
        append(
            &store,
            key("nested", "main"),
            vec![record(
                sequence,
                EventKind::BranchCreate(BranchCreateData {
                    at: BranchCreateAt::Sequence { sequence: cut },
                    source: BranchSource::Host {
                        operation_id: branch.into(),
                    },
                    branch: branch.into(),
                    prompt: String::new(),
                }),
            )],
        )
        .await
        .unwrap();
    }
    let main = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("nested", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    dim_agent_runtime::core::close_timeline(&main)
        .await
        .unwrap();
    let early = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("nested", "early"),
        vec![("main".into(), 2)],
        store.clone(),
    )
    .await
    .unwrap();
    let early_context = read_context(&early, 0).await.unwrap();
    assert!(early_context.checkpoint.is_none());
    assert_eq!(early_context.events.len(), 2);
    assert_eq!(
        project_context_history(&early_context).unwrap()[0].message,
        Message::text(Role::User, "earlier input")
    );
    let child = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("nested", "child"),
        vec![("main".into(), 4)],
        store.clone(),
    )
    .await
    .unwrap();
    child
        .append(
            EventKind::BranchStart(BranchStartData {
                parent: Some("main".into()),
                create_event: Some(6),
            })
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: "event-3".into(),
            })
            .into(),
        )
        .unwrap();
    child
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Sequence { sequence: 2 },
                source: BranchSource::Host {
                    operation_id: "grandchild".into(),
                },
                branch: "grandchild".into(),
                prompt: String::new(),
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&child)
        .await
        .unwrap();
    let grandchild = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("nested", "grandchild"),
        vec![("main".into(), 4), ("child".into(), 2)],
        store.clone(),
    )
    .await
    .unwrap();
    let context = read_context(&grandchild, 0).await.unwrap();
    assert!(context.checkpoint.is_some());
    assert_eq!(
        context
            .positions
            .iter()
            .map(|position| (position.branch_id.as_str(), position.sequence))
            .collect::<Vec<_>>(),
        [("main", 3), ("main", 4), ("child", 1), ("child", 2)]
    );
    assert_eq!(
        context_messages(&context, &project_context_history(&context).unwrap()),
        [
            dim_agent_capabilities::history::summary_message("earlier summary"),
            Message::text(Role::User, "queued across compaction")
        ]
    );
    let history = grandchild.history(None).await.unwrap();
    assert_eq!(history.len(), 6);
    assert_eq!(history[..4], main.local(..=4).await.unwrap());
    assert_eq!(history[4..], child.local(..=2).await.unwrap());
    assert_eq!(store.branches("nested").await.unwrap().len(), 4);
    dim_agent_runtime::core::close_timeline(&early)
        .await
        .unwrap();
    dim_agent_runtime::core::close_timeline(&grandchild)
        .await
        .unwrap();
    dim_agent_runtime::core::close_timeline(&prefix)
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn failed_branch_cuts_remain_readable_as_original_durable_facts() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let mut originals = vec![];
    for bad_cut in [1, 90] {
        let session = format!("failed-cut-{bad_cut}");
        store
            .create_session(&session, &SessionConfig::default())
            .await
            .unwrap();
        let ctx = rutis::Ctx::root().unwrap();
        let timeline = Timeline::new(ctx.clone(), key(&session, "main"), vec![], store.clone());
        timeline
            .append_batch_registered(
                vec![
                    EventKind::StopInterrupt(StopInterruptData {
                        reason: "filler".into(),
                        scope: InterruptScope::Turn,
                    })
                    .into(),
                    EventKind::StopInterrupt(StopInterruptData {
                        reason: "filler".into(),
                        scope: InterruptScope::Turn,
                    })
                    .into(),
                ],
                |_| Ok(()),
            )
            .unwrap();
        let before = timeline
            .before(&ctx, "invalid-cut", &["branch/create"], move |mut draft| {
                let EventKind::BranchCreate(data) = &mut draft.kind else {
                    panic!()
                };
                data.at = BranchCreateAt::Sequence { sequence: bad_cut };
                Ok(draft)
            })
            .unwrap();
        let created = timeline
            .append(
                EventKind::BranchCreate(BranchCreateData {
                    at: BranchCreateAt::Sequence { sequence: 2 },
                    source: BranchSource::Host {
                        operation_id: "cut-check".into(),
                    },
                    branch: "failed-child".into(),
                    prompt: "candidate".into(),
                })
                .into(),
            )
            .unwrap();
        let failed = timeline
            .append(
                EventKind::BranchCreateFailed(BranchCreateFailedData::Failed {
                    branch: "failed-child".into(),
                    create_event: created.sequence,
                    error: "invalid branch cut".into(),
                })
                .into(),
            )
            .unwrap();
        dim_agent_runtime::core::close_timeline(&timeline)
            .await
            .unwrap();
        originals.push((session, created, failed));
        before.dispose().await.unwrap();
        drop(timeline);
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    for ((session, original, failed), bad_cut) in originals.iter().zip([1, 90]) {
        let branches = store.branches(session).await.unwrap();
        let child = branches
            .iter()
            .find(|branch| branch.info.id == "failed-child")
            .unwrap();
        assert_eq!(child.info.branch_point, Some(bad_cut));
        assert_eq!(child.sequence, 0);
        let events = store.read(&key(session, "main"), 3..=4).await.unwrap();
        assert_eq!(events[0].event, *original);
        assert_eq!(events[1].event, *failed);
        let trace = store
            .trace(&key(session, "main"), 3)
            .await
            .unwrap()
            .unwrap();
        let EventKind::BranchCreate(original) = trace.original.kind else {
            panic!()
        };
        assert_eq!(original.at, BranchCreateAt::Sequence { sequence: 2 });
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cold_read_failure_preserves_the_record_and_other_session_progress() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let mut builder = dim_agent_runtime::AgentBuilder::new(dim_agent_runtime::AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        builder.timeline_store(store.clone());
        let agent = builder.build();
        let bad = agent.create_session(SessionConfig::default()).await.unwrap();
        let good = agent.create_session(SessionConfig::default()).await.unwrap();
        bad.submit("stored request".into(), InputMode::Queue, "main").await.unwrap();
        bad.idle("main").await.unwrap();
        let connection = Connection::open(&database.0).unwrap();
        assert!(connection.execute("UPDATE timeline_content SET body = 'unreadable external bytes' WHERE session_id = ?1", [bad.id()]).unwrap() > 0);
        assert!(matches!(bad.history("main", None).await, Err(AgentError::Storage(_))));
        assert!(bad.failure().is_none(), "a read error belongs to its operation");
        good.submit("healthy progress".into(), InputMode::Queue, "main").await.unwrap();
        good.idle("main").await.unwrap();
        assert!(good.history("main", None).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("healthy progress"))));
        assert_eq!(connection.query_row("SELECT body FROM timeline_content WHERE session_id = ?1 LIMIT 1", [bad.id()], |row| row.get::<_, String>(0)).unwrap(), "unreadable external bytes");
        agent.close().await.unwrap();
        assert!(matches!(store.branches(bad.id()).await, Err(AgentError::Storage(_))));
        store.branches(good.id()).await.unwrap();
        drop(connection);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }).await.expect("the read failure remained local to the operation");
}

#[tokio::test]
async fn reads_share_bounded_content_across_pages_and_still_check_every_load() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    // Room for one request's content, and below it: each load evicts the
    // one before it.
    let fitting = SqliteTimelineStore::with_content_budget(db.clone(), 256);
    let small = SqliteTimelineStore::with_content_budget(db.clone(), 150);
    // One request of two ~100-byte messages, called on 300 sequences: reads
    // cross 256-sequence pages and validation crosses 64-row pages. `lists`
    // keeps the v17 rows; `legacy` holds the same records as a v16 daemon
    // wrote them.
    let request = Arc::new(ModelRequest::new(
        "mock",
        "test",
        ["first", "second"]
            .map(|name| Message::text(Role::User, format!("{name} {}", "x".repeat(90))))
            .into(),
    ));
    let records: Vec<_> = (1..=300)
        .map(|sequence| {
            record(
                sequence,
                EventKind::ModelCall(ModelCallData {
                    turn_id: "turn".into(),
                    request_id: format!("request-{sequence}"),
                    request: request.clone(),
                    configuration: None,
                    prompt: None,
                }),
            )
        })
        .collect();
    for session in ["lists", "legacy"] {
        store
            .create_session(session, &SessionConfig::default())
            .await
            .unwrap();
        append(&store, key(session, "main"), records.clone())
            .await
            .unwrap();
    }
    let connection = Connection::open(&database.0).unwrap();
    as_v16_rows(&connection, "legacy");
    let message = |record: &StoredEvent| match &record.event.kind {
        EventKind::ModelCall(data) => data.request.messages[0].clone(),
        _ => panic!("not a model call"),
    };
    // A v17 read shares the kept request's messages whatever its budget; a
    // v16 read shares one decode unless the budget evicted it.
    for (session, budget_shares) in [("lists", true), ("legacy", false)] {
        for (reader, shares) in [(&store, true), (&fitting, true), (&small, budget_shares)] {
            let page = reader
                .read_page(&key(session, "main"), 0, 300, 300)
                .await
                .unwrap();
            let segment = reader
                .read_segments(session, vec![("main".into(), 1..=300)])
                .await
                .unwrap()
                .pop()
                .unwrap();
            for read in [&page, &segment] {
                assert!(
                    read.iter()
                        .map(|record| &record.event)
                        .eq(records.iter().map(|record| &record.event))
                );
                assert_eq!(
                    Arc::ptr_eq(&message(&read[0]), &message(&read[299])),
                    shares
                );
            }
            reader.branches(session).await.unwrap();
        }
    }

    // A damaged reference or content row still fails every read and the
    // recovery validation. The reference is damaged on sequence 299, past
    // the first page and before `through`, after the same content was
    // loaded intact: in `lists` the list's last node, in `legacy` the first
    // message.
    let query = |sql: &str, session: &str| -> String {
        connection
            .query_row(sql, [session], |row| row.get(0))
            .unwrap()
    };
    for (session, reference) in [
        (
            "lists",
            query(
                "SELECT hash FROM timeline_content WHERE session_id = ?1 AND kind = 'node' AND body LIKE '%second%'",
                "lists",
            ),
        ),
        (
            "legacy",
            query(
                "SELECT hash FROM timeline_content WHERE session_id = ?1 AND kind = 'message' AND body LIKE '%first%'",
                "legacy",
            ),
        ),
    ] {
        let event = query(
            "SELECT body FROM timeline_events WHERE session_id = ?1 AND sequence = 299",
            session,
        );
        assert!(event.contains(&reference));
        let first = query(
            "SELECT body FROM timeline_content WHERE session_id = ?1 AND kind IN ('node', 'message') AND body LIKE '%first%'",
            session,
        );
        for (table, intact, damaged, failure) in [
            (
                "timeline_events",
                event.clone(),
                event.replace(&reference, &reference.to_uppercase()),
                "missing request content",
            ),
            (
                "timeline_content",
                first.clone(),
                first.replace("first", "fir5t"),
                "request content hash mismatch",
            ),
        ] {
            let set = format!("UPDATE {table} SET body = ?1 WHERE session_id = ?2 AND body = ?3");
            assert_eq!(
                connection
                    .execute(&set, [&damaged, session, &intact])
                    .unwrap(),
                1
            );
            for reader in [&store, &fitting, &small] {
                assert!(matches!(
                    reader.read_page(&key(session, "main"), 0, 300, 300).await,
                    Err(AgentError::Storage(message)) if message.contains(failure)
                ));
                assert!(matches!(
                    reader.read(&key(session, "main"), 1..=300).await,
                    Err(AgentError::Storage(message)) if message.contains(failure)
                ));
                assert!(matches!(
                    reader.branches(session).await,
                    Err(AgentError::Storage(message)) if message.contains(failure)
                ));
            }
            // The bad row kept its bytes.
            assert_eq!(
                connection
                    .execute(&set, [&intact, session, &damaged])
                    .unwrap(),
                1
            );
        }
        store.branches(session).await.unwrap();
    }
    drop(connection);
    small.close().await.unwrap();
    fitting.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn directory_history_pages_keep_a_fixed_head_and_resolve_prior_tool_calls() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions =
        dim_app_server::storage::SessionStore::new(db.clone(), rutis::Ctx::root().unwrap(), 0);
    let store = sessions.timeline_store();
    store
        .create_session("paged", &SessionConfig::default())
        .await
        .unwrap();
    let mut records = vec![record(
        1,
        EventKind::ToolCall(ToolCallData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            calls: vec![ToolCall {
                id: "call".into(),
                name: "echo".into(),
                input: serde_json::Map::new(),
            }],
        }),
    )];
    records.extend((2..=600).map(|sequence| text(sequence, "delta")));
    let (done, wait) = tokio::sync::oneshot::channel();
    store
        .append(
            key("paged", "main"),
            records,
            Box::new(move |result| {
                let _ = done.send(result);
            }),
        )
        .unwrap();
    wait.await.unwrap().unwrap();
    let through = store.head(&key("paged", "main")).await.unwrap();
    let first = sessions.read_page("paged", 0, 200, through).await.unwrap();
    assert_eq!(first.len(), 200);
    let (done, wait) = tokio::sync::oneshot::channel();
    store
        .append(
            key("paged", "main"),
            vec![text(601, "later")],
            Box::new(move |result| {
                let _ = done.send(result);
            }),
        )
        .unwrap();
    wait.await.unwrap().unwrap();
    let second = sessions
        .read_page("paged", 200, 200, through)
        .await
        .unwrap();
    let third = sessions
        .read_page("paged", 400, 200, through)
        .await
        .unwrap();
    let sequences: Vec<_> = first
        .iter()
        .chain(&second)
        .chain(&third)
        .map(|event| event.sequence)
        .collect();
    assert_eq!(sequences, (1..=600).collect::<Vec<_>>());
    assert!(
        sessions
            .read_page("paged", 600, 200, through)
            .await
            .unwrap()
            .is_empty()
    );
    let call = sessions
        .read_event("paged", "event-1")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&call.kind, EventKind::ToolCall(data) if data.calls[0].name == "echo"));
    let connection = Connection::open(&database.0).unwrap();
    let plan: String = connection.query_row(
        "EXPLAIN QUERY PLAN SELECT sequence FROM timeline_events WHERE session_id='paged' AND branch_id='main' AND last_sequence>=201 AND sequence<=400 ORDER BY last_sequence",
        [], |row| row.get(3)).unwrap();
    assert!(plan.contains("timeline_events_by_end"), "{plan}");
    drop(connection);
    drop(store);
    drop(sessions);
    db.close().await.unwrap();
}

/// A call's `step/tool-use-start` is found by request and call id through its
/// index, however far it is from the result closing the call.
#[tokio::test]
async fn tool_use_starts_are_found_by_request_and_call_id() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions =
        dim_app_server::storage::SessionStore::new(db.clone(), rutis::Ctx::root().unwrap(), 0);
    let store = sessions.timeline_store();
    store
        .create_session("starts", &SessionConfig::default())
        .await
        .unwrap();
    let start = |sequence, request_id: &str, call_id: &str, name: &str| {
        record(
            sequence,
            EventKind::ToolUseStart(ToolUseStartData {
                turn_id: "turn".into(),
                request_id: request_id.into(),
                call_id: call_id.into(),
                name: name.into(),
            }),
        )
    };
    let mut records = vec![
        start(1, "request", "call", "echo"),
        start(2, "other", "call", "wait"),
    ];
    records.extend((3..=600).map(|sequence| text(sequence, "delta")));
    let (done, wait) = tokio::sync::oneshot::channel();
    store
        .append(
            key("starts", "main"),
            records,
            Box::new(move |result| {
                let _ = done.send(result);
            }),
        )
        .unwrap();
    wait.await.unwrap().unwrap();
    let name = async |request_id, call_id| {
        sessions
            .read_tool_use_start("starts", request_id, call_id)
            .await
            .unwrap()
            .map(|event| match &event.kind {
                EventKind::ToolUseStart(data) => data.name.clone(),
                kind => panic!("unexpected {}", kind.key()),
            })
    };
    assert_eq!(name("request", "call").await.as_deref(), Some("echo"));
    assert_eq!(name("other", "call").await.as_deref(), Some("wait"));
    assert_eq!(name("request", "missing").await, None);
    let connection = Connection::open(&database.0).unwrap();
    let plan: String = connection.query_row(
        "EXPLAIN QUERY PLAN SELECT sequence FROM timeline_events WHERE session_id='starts' AND branch_id='main' AND kind='step/tool-use-start' AND json_extract(body, '$.record.event.data.request_id')='request' AND json_extract(body, '$.record.event.data.call_id')='call'",
        [], |row| row.get(3)).unwrap();
    assert!(plan.contains("timeline_tool_use_starts"), "{plan}");
    drop(connection);
    drop(store);
    drop(sessions);
    db.close().await.unwrap();
}

#[tokio::test]
async fn history_pages_preserve_batches_in_memory_and_sqlite() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sqlite = SqliteTimelineStore::new(db.clone());
    let stores: Vec<Arc<dyn TimelineStore>> = vec![
        Arc::new(dim_agent_runtime::MemoryTimelineStore::default()),
        sqlite.clone(),
    ];
    for store in stores {
        let key = key("batches", "main");
        store
            .create_session("batches", &SessionConfig::default())
            .await
            .unwrap();
        let records = (1..=303)
            .map(|sequence| {
                let mut event = if sequence == 1 {
                    record(
                        sequence,
                        EventKind::InputMessage(InputMessageData::text(
                            "first batch",
                            Some(InputMode::Queue),
                        )),
                    )
                } else {
                    text(sequence, "delta")
                };
                event.batch_end = if sequence <= 300 { 300 } else { 303 };
                event
            })
            .collect();
        let (done, wait) = tokio::sync::oneshot::channel();
        store
            .append(
                key.clone(),
                records,
                Box::new(move |result| {
                    let _ = done.send(result);
                }),
            )
            .unwrap();
        wait.await.unwrap().unwrap();
        let first = store.read_page(&key, 0, 2, 303).await.unwrap();
        assert_eq!(
            first.len(),
            300,
            "one batch may cross rows and internal read segments"
        );
        let second = store.read_page(&key, 300, 1, 303).await.unwrap();
        assert_eq!(
            second.iter().map(|r| r.event.sequence).collect::<Vec<_>>(),
            vec![301, 302, 303]
        );
        assert!(matches!(
            store.read_page(&key, 1, 2, 303).await,
            Err(AgentError::InvalidInput(_))
        ));
        assert!(matches!(
            store.read_page(&key, 0, 2, 299).await,
            Err(AgentError::InvalidInput(_))
        ));
        assert!(store.read_page(&key, 0, 0, 303).await.unwrap().is_empty());
        assert!(store.read_page(&key, 303, 2, 303).await.unwrap().is_empty());
    }
    sqlite.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn creation_rollback_drains_accepted_writes_and_deletes_atomically() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("new", &SessionConfig::default())
        .await
        .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_rollback BEFORE DELETE ON timeline_branches BEGIN SELECT RAISE(ABORT, 'rollback failure'); END").unwrap();
    let (done, wait) = tokio::sync::oneshot::channel();
    store
        .append(
            key("new", "main"),
            vec![text(1, "pending before rollback")],
            Box::new(move |result| {
                let _ = done.send(result);
            }),
        )
        .unwrap();
    let error = store.rollback_session("new").await.unwrap_err();
    assert!(error.to_string().contains("rollback failure"));
    wait.await.unwrap().unwrap();
    assert!(store.session_config("new").await.unwrap().is_some());
    assert_eq!(
        store.read(&key("new", "main"), 1..=1).await.unwrap().len(),
        1,
        "failed deletion must roll back the preceding event deletion"
    );
    // A request the runtime still holds when its Session is rolled back.
    let history = request_history();
    let (_, first) = &history[1];
    append(&store, key("new", "main"), vec![first.clone()])
        .await
        .unwrap();
    assert_eq!(store.writer_memo_requests("new"), 1);
    connection
        .execute_batch("DROP TRIGGER fail_rollback")
        .unwrap();
    store.rollback_session("new").await.unwrap();
    assert_eq!(store.writer_memo_requests("new"), 0);
    for table in [
        "agent_sessions",
        "timeline_branches",
        "timeline_events",
        "timeline_content",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    assert!(store.session_config("new").await.unwrap().is_none());
    // A new Session of the same id shares no list with the rolled-back
    // request: its request extending that one writes every node.
    store
        .create_session("new", &SessionConfig::default())
        .await
        .unwrap();
    let extending = record(3, history[3].1.event.kind.clone());
    let written = vec![text(1, "again"), text(2, "again"), extending];
    for record in &written {
        append(&store, key("new", "main"), vec![record.clone()])
            .await
            .unwrap();
    }
    let read = store.read(&key("new", "main"), 1..=3).await.unwrap();
    assert_eq!(
        read.iter().map(|record| &record.event).collect::<Vec<_>>(),
        written
            .iter()
            .map(|record| &record.event)
            .collect::<Vec<_>>()
    );
    drop(connection);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn native_continuation_survives_sqlite_checkpoint_request_and_child_restore() {
    use dim_agent_capabilities::history::{project_context_history, projection_checkpoint};
    use serde_json::json;
    for metadata in [
        json!({"protocol":"anthropic","signature":"signature-only"}),
        json!({"protocol":"gemini","thoughtSignature":"thought-signature"}),
        json!({"protocol":"openai-responses","itemId":"rs-1","encryptedContent":"encrypted-final","summaryIndex":0}),
    ] {
        let content: Vec<MessagePart> = serde_json::from_value(json!([
            {"type":"thinking","thinking":"","providerMetadata":metadata},
            {"type":"text","text":"before"},
            {"type":"text","text":"after"},
        ]))
        .unwrap();
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("native", &SessionConfig::default())
            .await
            .unwrap();
        append(
            &store,
            key("native", "main"),
            vec![
                record(
                    1,
                    EventKind::InputMessage(InputMessageData::text(
                        "input",
                        Some(InputMode::Queue),
                    )),
                ),
                record(
                    2,
                    EventKind::InputConsumed(InputConsumedData {
                        input_id: "event-1".into(),
                    }),
                ),
                record(
                    3,
                    EventKind::ModelStart(ModelStartData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                    }),
                ),
                text(4, "beforeafter"),
                record(
                    5,
                    EventKind::ModelContent(ModelContentData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        content: content.clone(),
                    }),
                ),
                record(
                    6,
                    EventKind::ModelOutputEnd(ModelOutputEndData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        outcome: ModelOutputOutcome::Completed {
                            finish_reason: FinishReason::EndTurn,
                        },
                    }),
                ),
            ],
        )
        .await
        .unwrap();
        let timeline = Timeline::restore(
            rutis::Ctx::root().unwrap(),
            key("native", "main"),
            vec![],
            store.clone(),
        )
        .await
        .unwrap();
        let checkpoint =
            projection_checkpoint(&timeline.context(6).await.unwrap(), "event-3").unwrap();
        assert_eq!(checkpoint.state.output, content);
        let compaction = completed("event-3", "summary", 6);
        append(
            &store,
            key("native", "main"),
            vec![record(7, EventKind::ContextCompaction(compaction))],
        )
        .await
        .unwrap();
        store
            .save_snapshot(&key("native", "main"), snapshot(7, &checkpoint))
            .await
            .unwrap();
        let assistant = Message {
            role: Role::Assistant,
            content: MessageContent::Parts(content.clone()),
        };
        append(
            &store,
            key("native", "main"),
            vec![
                record(
                    8,
                    EventKind::ModelCall(ModelCallData {
                        turn_id: "next".into(),
                        request_id: "next".into(),
                        request: Arc::new(ModelRequest::new(
                            "native",
                            "model",
                            vec![assistant.clone()],
                        )),
                        configuration: None,
                        prompt: None,
                    }),
                ),
                record(
                    9,
                    EventKind::BranchCreate(BranchCreateData {
                        at: BranchCreateAt::Sequence { sequence: 8 },
                        source: BranchSource::Host {
                            operation_id: "child".into(),
                        },
                        branch: "child".into(),
                        prompt: String::new(),
                    }),
                ),
            ],
        )
        .await
        .unwrap();
        dim_agent_runtime::core::close_timeline(&timeline)
            .await
            .unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(timeline);
        drop(store);
        drop(db);
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        for (branch, ancestors, through) in
            [("main", vec![], 9), ("child", vec![("main".into(), 8)], 0)]
        {
            let timeline = Timeline::restore(
                rutis::Ctx::root().unwrap(),
                key("native", branch),
                ancestors,
                store.clone(),
            )
            .await
            .unwrap();
            let history =
                project_context_history(&read_context(&timeline, through).await.unwrap()).unwrap();
            assert_eq!(
                history
                    .iter()
                    .map(|entry| &entry.message)
                    .collect::<Vec<_>>(),
                vec![&assistant]
            );
            let events = timeline.history(None).await.unwrap();
            assert!(events.iter().any(|e|matches!(&e.kind,EventKind::ModelCall(call) if call.request.messages[0].as_ref()==&assistant)));
            dim_agent_runtime::core::close_timeline(&timeline)
                .await
                .unwrap();
        }
        let saved = store
            .latest_snapshot(&key("native", "main"), 9, INTERPRETER)
            .await
            .unwrap()
            .unwrap();
        let timeline = Timeline::restore(
            rutis::Ctx::root().unwrap(),
            key("native", "main"),
            vec![],
            store.clone(),
        )
        .await
        .unwrap();
        let anchor = timeline
            .local(saved.sequence..=saved.sequence)
            .await
            .unwrap()
            .pop()
            .unwrap();
        dim_agent_runtime::core::close_timeline(&timeline)
            .await
            .unwrap();
        let checkpoint = (HistoryRenderer::default().decode_checkpoint)(&saved, &anchor).unwrap();
        assert_eq!(checkpoint.state.output, content);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
}

/// S5c §6.3 at the store: a fork copies `main` through the cut as stored,
/// only the content rows its events and before traces reference, only the
/// snapshots anchored on copied events, and the branches `main` created by
/// then as directories without events; a missing source is closed.
#[tokio::test]
async fn a_fork_copies_main_through_the_cut_with_its_content_and_snapshots() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions =
        dim_app_server::storage::SessionStore::new(db.clone(), rutis::Ctx::root().unwrap(), 0);
    // Rows are written as the Timeline writer writes them.
    let store = SqliteTimelineStore::new(db.clone());
    let config = SessionConfig {
        interactive: false,
        ..SessionConfig::default()
    };
    store.create_session("source", &config).await.unwrap();
    let call = |sequence, texts: &[&str]| {
        record(
            sequence,
            EventKind::ModelCall(ModelCallData {
                turn_id: "turn".into(),
                request_id: format!("request-{sequence}"),
                request: Arc::new(ModelRequest::new(
                    "mock",
                    "test",
                    texts
                        .iter()
                        .map(|text| Message::text(Role::User, *text))
                        .collect(),
                )),
                configuration: None,
                prompt: None,
            }),
        )
    };
    let compaction = |sequence, through| {
        record(
            sequence,
            EventKind::ContextCompaction(completed("event-1", "summary", through)),
        )
    };
    let created = |sequence, branch: &str| {
        record(
            sequence,
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Create {},
                source: BranchSource::Host {
                    operation_id: "op".into(),
                },
                branch: branch.into(),
                prompt: "child prompt".into(),
            }),
        )
    };
    // `main` is mixed: 1 is a v16 row, as a v16 daemon wrote it before the
    // upgrade; a store started after it writes the rest as lists.
    append(&store, key("source", "main"), vec![call(1, &["first"])])
        .await
        .unwrap();
    store.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    as_v16_rows(&connection, "source");
    let store = SqliteTimelineStore::new(db.clone());
    // 2 carries a before trace whose draft holds its own request.
    let mut second = call(2, &["first", "before the cut"]);
    let EventKind::ModelCall(traced) = &second.event.kind else {
        unreachable!()
    };
    let mut traced = traced.clone();
    traced.request = Arc::new(ModelRequest::new(
        "mock",
        "test",
        vec![Message::text(Role::User, "only in the trace")],
    ));
    second.trace = Some(TimelineTrace {
        event_id: second.event.id.clone(),
        original: EventDraft::new(second.event.kind.clone()),
        steps: vec![TraceStep {
            handler: "before".into(),
            result: Some(EventDraft::new(EventKind::ModelCall(traced))),
        }],
    });
    // 5 extends 2's list past the cut.
    for records in [
        vec![second],
        vec![compaction(3, 2)],
        vec![created(4, "child")],
        vec![call(5, &["first", "before the cut", "after the cut"])],
        vec![compaction(6, 5)],
        vec![created(7, "later")],
    ] {
        append(&store, key("source", "main"), records)
            .await
            .unwrap();
    }
    append(
        &store,
        key("source", "child"),
        vec![
            record(
                1,
                EventKind::BranchStart(BranchStartData {
                    parent: Some("main".into()),
                    create_event: Some(4),
                }),
            ),
            call(2, &["child request"]),
        ],
    )
    .await
    .unwrap();
    for sequence in [3, 6] {
        store
            .save_snapshot(
                &key("source", "main"),
                RecoverySnapshot {
                    sequence,
                    event_id: format!("event-{sequence}"),
                    interpreter: INTERPRETER.into(),
                    state: "state".into(),
                },
            )
            .await
            .unwrap();
    }

    sessions.fork_session("source", "fork", 4).await.unwrap();
    let column = |sql: &str| -> Vec<String> {
        connection
            .prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    // The copied rows, then the fork's own fact in the same transaction.
    assert_eq!(
        column(
            "SELECT sequence || ':' || body FROM timeline_events WHERE session_id='fork' AND sequence<=4 ORDER BY sequence"
        ),
        column(
            "SELECT sequence || ':' || body FROM timeline_events WHERE session_id='source' AND branch_id='main' AND sequence<=4 ORDER BY sequence"
        )
    );
    let forked = column(
        "SELECT branch_id || ':' || kind || ':' || body FROM timeline_events WHERE session_id='fork' AND sequence>4",
    );
    assert_eq!(forked.len(), 1, "{forked:?}");
    assert!(
        forked[0].starts_with("main:session/forked:")
            && forked[0].contains(r#""type":"session","name":"forked","data":{}"#),
        "{forked:?}"
    );
    assert_eq!(
        column(
            "SELECT branch_id || ':' || sequence || ':' || info FROM timeline_branches WHERE session_id='fork' ORDER BY branch_id"
        ),
        column(
            "SELECT branch_id || ':' || CASE branch_id WHEN 'main' THEN 5 ELSE 0 END || ':' || info FROM timeline_branches WHERE session_id='source' AND branch_id IN ('main', 'child') ORDER BY branch_id"
        )
    );
    // Exactly the closure of the copied rows' references: 1's message, and
    // every node of 2's list and of its draft's, but no node only a request
    // after the cut or another branch reaches.
    let content = column(
        "SELECT kind || ':' || json_extract(body, '$.content') FROM timeline_content WHERE session_id='fork' AND kind='message' \
         UNION ALL SELECT kind || ':' || json_extract(body, '$.message.content') FROM timeline_content WHERE session_id='fork' AND kind='node' ORDER BY 1",
    );
    assert_eq!(
        content,
        [
            "message:first",
            "node:before the cut",
            "node:first",
            "node:only in the trace"
        ]
    );
    assert_eq!(
        column("SELECT count(*) || '' FROM timeline_content WHERE session_id='fork'"),
        ["4"]
    );
    assert_eq!(
        store
            .read(&key("fork", "main"), 1..=4)
            .await
            .unwrap()
            .iter()
            .map(|record| (
                record.event.clone(),
                serde_json::to_value(&record.trace).unwrap()
            ))
            .collect::<Vec<_>>(),
        store
            .read(&key("source", "main"), 1..=4)
            .await
            .unwrap()
            .iter()
            .map(|record| (
                record.event.clone(),
                serde_json::to_value(&record.trace).unwrap()
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        column(
            "SELECT session_id || ':' || sequence FROM timeline_snapshots WHERE session_id='fork'"
        ),
        ["fork:3"]
    );
    assert_eq!(
        column("SELECT config FROM agent_sessions WHERE session_id='fork'"),
        column("SELECT config FROM agent_sessions WHERE session_id='source'")
    );
    // The copy is a valid Session: the child is a creation that never
    // started, and the fork's main resumes after its `session/forked`.
    let branches = store.branches("fork").await.unwrap();
    assert_eq!(
        branches
            .iter()
            .map(|branch| (branch.info.id.as_str(), branch.sequence))
            .collect::<Vec<_>>(),
        [("child", 0), ("main", 5)]
    );
    assert_eq!(
        store
            .latest_snapshot(&key("fork", "main"), 5, INTERPRETER)
            .await
            .unwrap()
            .map(|snapshot| snapshot.sequence),
        Some(3)
    );

    assert!(matches!(
        sessions.fork_session("missing", "orphan", 0).await,
        Err(AgentError::Closed(_))
    ));
    assert!(column("SELECT session_id FROM agent_sessions WHERE session_id='orphan'").is_empty());
    drop((connection, sessions));
    store.close().await.unwrap();
    db.close().await.unwrap();
}

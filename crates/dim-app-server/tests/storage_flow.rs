#[path = "storage_flow/recall.rs"]
mod recall;
#[path = "storage_flow/state_indexes.rs"]
mod state_indexes;

#[path = "storage_flow/array_reads.rs"]
mod array_reads;
#[path = "storage_flow/member_queries.rs"]
mod member_queries;
#[path = "storage_flow/property_queries.rs"]
mod property_queries;

use dim_agent_capabilities::file_storage::FileStoragePlugin;
use dim_agent_capabilities::history::HistoryRenderer;
use dim_agent_runtime::testing::{chain_events, timeline_events};
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, AgentError, MemoryTimelineStore, SessionConfig, StoredEvent,
    Timeline, TimelineKey, TimelineQueryScope, TimelineStore, TimelineTrace, TraceStep,
    testing::MinimalHost,
};
use dim_agent_types::{
    grant::{GrantKind, GrantMode, SessionGrant},
    *,
};
use dim_app_server::protocol::SessionOrigin;
use dim_app_server::storage::{
    NodeLoads, STORE_FORMAT, SqliteStorage, SqliteTimelineStore, vacuum_store,
};
use rusqlite::{Connection, params};
use rutis::host::CtxHost;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[path = "storage_flow/catalog.rs"]
mod catalog;

#[path = "storage_flow/worktrees.rs"]
mod worktrees;

#[path = "storage_flow/queue_recovery.rs"]
mod queue_recovery;

#[path = "storage_flow/prompt_recovery.rs"]
mod prompt_recovery;

const INTERPRETER: &str = "dim/context-projection/v4";
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
            replacement_parts: None,
            pinned_inputs: None,
            digest_entries: None,
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
            parent_call_id: None,
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
fn text_with_id(sequence: u64, delta: &str, id: &str) -> StoredEvent {
    let mut entry = text(sequence, delta);
    Arc::make_mut(&mut entry.event).id = id.into();
    entry
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
const PUBLISHED: Duration = Duration::from_secs(30);
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
/// How long a flow waits for the Session fact it asserts on, such as a close,
/// before it fails instead of hanging.
const SETTLED: Duration = Duration::from_secs(10);
/// Waits for `fact` and fails the flow with `what` when it never holds.
async fn settled<T>(what: &str, fact: impl Future<Output = T>) -> T {
    tokio::time::timeout(SETTLED, fact)
        .await
        .unwrap_or_else(|_| panic!("the flow never observed that {what}"))
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
    "session_grants",
    "session_output_owners",
    "worktree_directories",
    "session_worktrees",
    "session_metadata",
    "session_catalog",
];
/// The business tables each upgrade step adds: v15's, then v16's, v28's and
/// v29's; v17, v18, v19 through v22 and v24 add none; v27 only alters columns.
const V15_TABLES: [&str; 2] = ["session_files", "session_file_owners"];
const V16_TABLES: [&str; 1] = ["session_metadata"];
const V28_TABLES: [&str; 1] = ["session_grants"];
const V29_TABLES: [&str; 1] = ["session_output_owners"];
const V33_TABLES: [&str; 2] = ["worktree_directories", "session_worktrees"];
/// The columns `table` has, so a record example written for a later format
/// contributes only the cells an older table can hold.
fn table_columns(connection: &Connection, table: &str) -> Vec<String> {
    connection
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

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
            let present = table_columns(connection, table);
            let row: serde_json::Map<String, serde_json::Value> = row
                .iter()
                .filter(|(column, _)| present.iter().any(|name| name == *column))
                .map(|(column, value)| (column.clone(), value.clone()))
                .collect();
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

/// The documented example predates the two context switches (v31): a store
/// that holds it has them set from each row's kind, as the upgrade writes them.
fn write_documented_context_switches(connection: &Connection) {
    let keys = |list: &[&str]| {
        list.iter()
            .map(|key| format!("'{key}'"))
            .collect::<Vec<_>>()
            .join(",")
    };
    connection
        .execute_batch(&format!(
            "UPDATE timeline_events SET context_render=1 WHERE kind IN ({});",
            keys(dim_agent_types::CONTEXT_RENDER_KEYS)
        ))
        .unwrap();
    connection
        .execute_batch(&format!(
            "UPDATE timeline_events SET context_state=1 WHERE kind IN ({});",
            keys(dim_agent_types::CONTEXT_STATE_KEYS)
        ))
        .unwrap();
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
/// The v26 text of the two tables v27 adds a column to, and what v27 adds to
/// them besides the marker.
const AGENT_SESSIONS_V25: &str = "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0))";
const AGENT_SESSIONS_V27: &str = "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0), leader_session_id TEXT REFERENCES agent_sessions(session_id))";
const AGENT_SESSIONS_V30: &str = "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0), leader_session_id TEXT REFERENCES agent_sessions(session_id), origin TEXT NOT NULL DEFAULT 'unknown' CHECK (origin IN ('client', 'acp', 'internal', 'unknown')))";
const AGENT_SESSIONS_V42: &str = "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0), leader_session_id TEXT REFERENCES agent_sessions(session_id), origin TEXT NOT NULL DEFAULT 'unknown' CHECK (origin IN ('client', 'acp', 'internal', 'unknown', 'automation')))";
const AGENTS_V25: &str = "CREATE TABLE agents(agent_id TEXT PRIMARY KEY, profile TEXT NOT NULL, is_default INTEGER NOT NULL CHECK(is_default IN (0, 1)))";
const AGENTS_V27: &str = "CREATE TABLE agents(agent_id TEXT PRIMARY KEY, profile TEXT NOT NULL, is_default INTEGER NOT NULL CHECK(is_default IN (0, 1)), owner_session_id TEXT REFERENCES agent_sessions(session_id))";
const V27_ADDITIONS: &[&str] = &[
    "CREATE INDEX agent_sessions_by_leader ON agent_sessions(leader_session_id)",
    "CREATE INDEX agents_by_owner ON agents(owner_session_id)",
];
const TIMELINE_EVENTS_V27: &str = "CREATE TABLE timeline_events (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), last_sequence INTEGER NOT NULL CHECK (last_sequence >= sequence), kind TEXT NOT NULL, body TEXT NOT NULL, event_id TEXT, PRIMARY KEY (session_id, branch_id, sequence), FOREIGN KEY (session_id, branch_id) REFERENCES timeline_branches(session_id, branch_id))";
const TIMELINE_EVENTS_V31: &str = "CREATE TABLE timeline_events (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), last_sequence INTEGER NOT NULL CHECK (last_sequence >= sequence), kind TEXT NOT NULL, body TEXT NOT NULL, event_id TEXT, context_render INTEGER NOT NULL DEFAULT 0, context_state INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (session_id, branch_id, sequence), FOREIGN KEY (session_id, branch_id) REFERENCES timeline_branches(session_id, branch_id))";
const V28_ADDITIONS: &[&str] = &[
    "CREATE TABLE session_grants (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), path TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('file', 'dir')), mode TEXT NOT NULL CHECK (mode IN ('read', 'write')), PRIMARY KEY (session_id, path))",
];
const V29_ADDITIONS: &[&str] = &[
    "CREATE TABLE session_output_owners (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), path TEXT NOT NULL, PRIMARY KEY (session_id, path))",
    "CREATE INDEX session_output_owners_by_path ON session_output_owners(path)",
];
const V31_ADDITIONS: &[&str] = &[
    "CREATE INDEX timeline_events_positions ON timeline_events(session_id, branch_id, kind, sequence) WHERE kind IN ('turn/start', 'context/compaction-start', 'context/compaction')",
];
/// The two indexes v31 added and v32 replaces with one.
const V31_REPLACED: &[&str] = &[
    "CREATE INDEX timeline_events_render ON timeline_events(session_id, branch_id, sequence) WHERE context_render = 1",
    "CREATE INDEX timeline_events_state ON timeline_events(session_id, branch_id, sequence) WHERE context_state = 1",
];
const V32_ADDITIONS: &[&str] = &[
    "CREATE INDEX timeline_events_context ON timeline_events(session_id, branch_id, sequence) WHERE context_render = 1 OR context_state = 1",
];
const V33_ADDITIONS: &[&str] = &[
    "CREATE TABLE worktree_directories (repository TEXT NOT NULL, root TEXT NOT NULL, PRIMARY KEY (repository, root))",
    "CREATE TABLE session_worktrees (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), repository TEXT NOT NULL, root TEXT NOT NULL, subtree TEXT NOT NULL, PRIMARY KEY (session_id, repository, root), FOREIGN KEY (repository, root) REFERENCES worktree_directories(repository, root))",
    "CREATE INDEX session_worktrees_by_directory ON session_worktrees(repository, root, session_id)",
];
/// The same tables from v42, whose `worktree_directories` carries the
/// last-used time; `schema_of` returns the schema a format actually has.
const V33_ADDITIONS_V43: &[&str] = &[
    "CREATE TABLE worktree_directories (repository TEXT NOT NULL, root TEXT NOT NULL, last_used_at_ms INTEGER, PRIMARY KEY (repository, root))",
    "CREATE TABLE session_worktrees (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), repository TEXT NOT NULL, root TEXT NOT NULL, subtree TEXT NOT NULL, PRIMARY KEY (session_id, repository, root), FOREIGN KEY (repository, root) REFERENCES worktree_directories(repository, root))",
    "CREATE INDEX session_worktrees_by_directory ON session_worktrees(repository, root, session_id)",
];
const V35_ADDITIONS: &[&str] = &[
    "CREATE INDEX timeline_input_submissions ON timeline_events(session_id, branch_id, json_extract(body, '$.record.event.data.submission_id'), sequence) WHERE kind = 'input/message'",
];
const V23_ADDITIONS: &[&str] = &[
    "CREATE TABLE session_catalog (session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id), updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0), preview TEXT, cwd TEXT NOT NULL)",
    "CREATE INDEX session_catalog_by_activity ON session_catalog(updated_at_ms DESC, session_id)",
    "CREATE INDEX session_catalog_with_preview ON session_catalog(updated_at_ms DESC, session_id) WHERE preview IS NOT NULL",
];
const V36_ADDITIONS: &[&str] = &[
    "CREATE INDEX timeline_events_requests ON timeline_events(session_id, branch_id, kind, coalesce(json_extract(body,'$.record.event.data.request_id'),json_extract(body,'$.request_id')), sequence)",
    "CREATE INDEX timeline_events_inputs ON timeline_events(session_id, branch_id, kind, json_extract(body,'$.record.event.data.input_id'), sequence)",
    "CREATE INDEX timeline_events_users ON timeline_events(session_id, branch_id, kind, json_type(body,'$.record.event.data.user'), sequence)",
    "CREATE TABLE timeline_text (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL, physical_sequence INTEGER NOT NULL, property TEXT NOT NULL, partition_key TEXT NOT NULL, UNIQUE(session_id, branch_id, sequence, property))",
    "CREATE INDEX timeline_text_partition ON timeline_text(session_id, branch_id, property, partition_key, sequence)",
    "CREATE VIRTUAL TABLE timeline_text_fts USING fts5(text, content='', contentless_delete=1, detail=none, tokenize='trigram case_sensitive 1')",
    "CREATE TRIGGER timeline_text_delete AFTER DELETE ON timeline_text BEGIN DELETE FROM timeline_text_fts WHERE rowid=old.id; END",
    "CREATE TABLE timeline_text_tails (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, request_id TEXT NOT NULL, tail TEXT NOT NULL, PRIMARY KEY(session_id, branch_id, request_id))",
];
const V37_ADDITIONS: &[&str] = &[
    "CREATE TABLE timeline_member_nodes (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence > 0), left_id INTEGER, right_id INTEGER, height INTEGER NOT NULL CHECK(height BETWEEN 1 AND 64))",
    "CREATE TABLE timeline_member_roots (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, index_id TEXT NOT NULL, group_key BLOB NOT NULL, sequence INTEGER NOT NULL CHECK(sequence >= 0), root_id INTEGER, PRIMARY KEY(session_id,branch_id,index_id,group_key,sequence))",
    "CREATE TABLE timeline_member_origins (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, index_id TEXT NOT NULL, match_key BLOB NOT NULL, sequence INTEGER NOT NULL CHECK(sequence > 0), group_key BLOB NOT NULL, PRIMARY KEY(session_id,branch_id,index_id,match_key,sequence,group_key))",
    "CREATE TABLE timeline_member_terminals (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, index_id TEXT NOT NULL, match_key BLOB NOT NULL, sequence INTEGER NOT NULL CHECK(sequence > 0), PRIMARY KEY(session_id,branch_id,index_id,match_key))",
];
const V39_ADDITIONS: &[&str] = &[
    "CREATE TABLE timeline_array_elements (session_id TEXT NOT NULL, array_id TEXT NOT NULL, element_index INTEGER NOT NULL CHECK(element_index >= 0), content_ref TEXT NOT NULL, array_length INTEGER, PRIMARY KEY(session_id, array_id, element_index), CHECK((element_index = 0 AND array_length IS NOT NULL AND array_length > 0) OR (element_index > 0 AND array_length IS NULL)))",
];
const V40_ADDITIONS: &[&str] = &[
    "CREATE TABLE timeline_output_ids (session_id TEXT NOT NULL,event_id TEXT NOT NULL,branch_id TEXT NOT NULL,physical_sequence INTEGER NOT NULL CHECK(physical_sequence > 0),logical_sequence INTEGER NOT NULL CHECK(logical_sequence >= physical_sequence),PRIMARY KEY(session_id,event_id))",
    "CREATE INDEX timeline_output_ids_by_position ON timeline_output_ids(session_id,branch_id,physical_sequence,logical_sequence,event_id)",
    "CREATE INDEX timeline_events_turns ON timeline_events(session_id,branch_id,kind,coalesce(json_extract(body,'$.record.event.data.turn_id'),json_extract(body,'$.turn_id')),sequence) WHERE coalesce(json_extract(body,'$.record.event.data.turn_id'),json_extract(body,'$.turn_id')) IS NOT NULL",
    "CREATE INDEX timeline_events_scopes ON timeline_events(session_id,branch_id,kind,json_extract(body,'$.record.event.data.scope.kind'),json_extract(body,'$.record.event.data.scope.timeline'),json_extract(body,'$.record.event.data.scope.id'),json_extract(body,'$.record.event.data.status'),sequence) WHERE kind='context/scope'",
    "CREATE INDEX timeline_events_source_positions ON timeline_events(session_id,branch_id,kind,json_extract(body,'$.record.event.data.epoch'),json_extract(body,'$.record.event.data.current.at.branch'),json_extract(body,'$.record.event.data.current.at.sequence'),sequence) WHERE kind='context-usage/recovered'",
];
fn drop_v40(connection: &Connection) {
    for sql in V40_ADDITIONS.iter().rev() {
        let mut words = sql.split_whitespace();
        words.next();
        let kind = words.next().unwrap();
        let name = words.next().unwrap();
        connection
            .execute_batch(&format!("DROP {kind} {name}"))
            .unwrap();
    }
    connection.execute_batch(&format!(
        "DROP INDEX session_owners_by_agent; ALTER TABLE session_owners RENAME TO session_owners_before_v41; {SESSION_OWNERS_V37}; INSERT INTO session_owners SELECT * FROM session_owners_before_v41; DROP TABLE session_owners_before_v41; CREATE INDEX session_owners_by_agent ON session_owners(agent_id, protocol_version, session_id);"
    )).unwrap();
    // A store from before v42 has no last-used column; the downgraded copies
    // those tests compare against `schema_of` below 42 must not carry it.
    connection
        .execute_batch("ALTER TABLE worktree_directories DROP COLUMN last_used_at_ms")
        .unwrap();
}
const SESSION_OWNERS_V37: &str = "CREATE TABLE session_owners(session_id TEXT PRIMARY KEY REFERENCES agent_sessions(session_id), agent_id TEXT NOT NULL REFERENCES agents(agent_id), protocol_version INTEGER NOT NULL CHECK(protocol_version IN (1, 2, 3)))";
const SESSION_OWNERS_V38: &str = "CREATE TABLE session_owners(session_id TEXT PRIMARY KEY REFERENCES agent_sessions(session_id), agent_id TEXT NOT NULL REFERENCES agents(agent_id), protocol_version INTEGER NOT NULL CHECK(protocol_version IN (1, 2, 3, 4)))";
/// Rewrites `agent_sessions` at its text before v42, as test fixtures do when
/// they take a live store back to an earlier format: #1421 widened the
/// column's CHECK, and the format gate compares the statement verbatim.
fn without_automation_origin(connection: &Connection) {
    connection
        .execute_batch(&format!(
            "PRAGMA foreign_keys=OFF;
             CREATE TEMP TABLE sessions_before_v42 AS SELECT * FROM agent_sessions;
             DROP INDEX agent_sessions_by_leader;
             DROP TABLE agent_sessions;
             {AGENT_SESSIONS_V30};
             CREATE INDEX agent_sessions_by_leader ON agent_sessions(leader_session_id);
             INSERT INTO agent_sessions SELECT session_id, config, created_at_ms, leader_session_id, origin FROM sessions_before_v42;
             DROP TABLE sessions_before_v42;"
        ))
        .unwrap();
}
fn schema_of(version: u32) -> Vec<String> {
    let mut statements: Vec<String> = V13_SCHEMA
        .iter()
        .chain(V15_ADDITIONS.iter().filter(|_| version >= 15))
        .chain(V16_ADDITIONS.iter().filter(|_| version >= 16))
        .chain(V23_ADDITIONS.iter().filter(|_| version >= 23))
        .chain(V27_ADDITIONS.iter().filter(|_| version >= 27))
        .chain(V28_ADDITIONS.iter().filter(|_| version >= 28))
        .chain(V29_ADDITIONS.iter().filter(|_| version >= 29))
        .chain(V31_REPLACED.iter().filter(|_| version == 31))
        .chain(V31_ADDITIONS.iter().filter(|_| version >= 31))
        .chain(V32_ADDITIONS.iter().filter(|_| version >= 32))
        .chain(V33_ADDITIONS.iter().filter(|_| (33..43).contains(&version)))
        .chain(V33_ADDITIONS_V43.iter().filter(|_| version >= 43))
        .chain(V35_ADDITIONS.iter().filter(|_| version >= 35))
        .chain(V36_ADDITIONS.iter().filter(|_| version >= 36))
        .chain(V37_ADDITIONS.iter().filter(|_| version >= 37))
        .chain(V39_ADDITIONS.iter().filter(|_| version >= 39))
        .chain(V40_ADDITIONS.iter().filter(|_| version >= 40))
        .filter(|sql| version <= 24 || !sql.starts_with("CREATE INDEX timeline_events_by_end "))
        .map(|sql| sql.replace("(version = 13)", &format!("(version = {version})")))
        .map(|sql| {
            if version >= 38 {
                match sql.as_str() {
                    sql if sql.starts_with("CREATE INDEX timeline_events_requests ") => format!("{sql} WHERE coalesce(json_extract(body,'$.record.event.data.request_id'),json_extract(body,'$.request_id')) IS NOT NULL"),
                    sql if sql.starts_with("CREATE INDEX timeline_events_inputs ") => format!("{sql} WHERE json_extract(body,'$.record.event.data.input_id') IS NOT NULL"),
                    sql if sql.starts_with("CREATE INDEX timeline_events_users ") => format!("{sql} WHERE json_type(body,'$.record.event.data.user') IS NOT NULL"),
                    _ => sql,
                }
            } else {
                sql
            }
        })
        .map(|sql| {
            let sql = if version < 27 {
                sql
            } else {
                sql.replace(AGENT_SESSIONS_V25, AGENT_SESSIONS_V27)
                    .replace(AGENTS_V25, AGENTS_V27)
            };
            if version >= 31 {
                sql.replace(TIMELINE_EVENTS_V27, TIMELINE_EVENTS_V31)
            } else {
                sql
            }
        })
        .map(|sql| {
            if version < 30 {
                sql
            } else {
                sql.replace(AGENT_SESSIONS_V27, AGENT_SESSIONS_V30)
            }
        })
        .map(|sql| {
            if version < 41 {
                sql
            } else {
                sql.replace(SESSION_OWNERS_V37, SESSION_OWNERS_V38)
            }
        })
        .map(|sql| {
            if version < 42 {
                sql
            } else {
                sql.replace(AGENT_SESSIONS_V30, AGENT_SESSIONS_V42)
            }
        })
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
/// created it, or a v17 or v18 daemon, which added no table.
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
/// Builds the v23 table layout under its v23 or v24 marker.
fn catalog_store(path: &std::path::Path, marker: u32) -> Connection {
    let connection = v16_tables_store(path, marker);
    for sql in V23_ADDITIONS {
        connection.execute_batch(sql).unwrap();
    }
    connection
}
/// Builds a disposable v25 store, as #674's daemon created it: the v24
/// layout without `timeline_events_by_end`.
fn v25_store(path: &std::path::Path) -> Connection {
    let connection = catalog_store(path, 25);
    connection
        .execute_batch("DROP INDEX timeline_events_by_end")
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
/// Whether the store has the schema object `name`.
/// The schema statements of a store, sorted.
fn schema(connection: &Connection) -> Vec<String> {
    let mut statements: Vec<String> = connection
        .prepare(
            "SELECT sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' \
             AND name NOT IN ('timeline_history_anchors', 'timeline_events_by_kind', \
                              'timeline_policy_turns')
             AND name NOT IN (SELECT name FROM pragma_table_list WHERE type='shadow' AND name LIKE 'timeline_text_fts_%')",
        )
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
    let projection = match table {
        "agents" => "agent_id, profile, is_default",
        "agent_sessions" => "session_id, config, created_at_ms",
        "timeline_events" => "session_id, branch_id, sequence, last_sequence, kind, body, event_id",
        // v43 adds the nullable last-used time; the identity rows are the
        // facts these upgrade tests compare.
        "worktree_directories" => "repository, root",
        _ => "*",
    };
    let mut query = connection
        .prepare(&format!("SELECT {projection} FROM {table}"))
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
    connection
        .execute("INSERT INTO agent_store_format VALUES (?1)", [STORE_FORMAT])
        .unwrap();
    insert_documented_records(&connection, BUSINESS_TABLES);
    write_documented_context_switches(&connection);
    for session in connection
        .prepare("SELECT session_id FROM agent_sessions")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
    {
        let session = session.unwrap();
        dim_app_server::storage::initialize_session_member_index(&connection, &session).unwrap();
        dim_app_server::storage::initialize_session_output_id_index(&connection, &session).unwrap();
    }
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
    let generated = projection_checkpoint(
        &dim_agent_capabilities::history::HistoryRenderer::default()
            .replay_context(&main, 2)
            .await
            .unwrap(),
        "e2",
    )
    .unwrap();
    let saved = store
        .latest_snapshot(&key("s1", "main"), 7, "dim/context-projection/v1")
        .await
        .unwrap()
        .unwrap();
    let saved: ProjectionCheckpoint = serde_json::from_str(&saved.state).unwrap();
    assert_eq!(saved, generated);
    let full = project_history(
        &timeline_events(&main, TimelineQueryScope::Chain, ..=7)
            .await
            .unwrap(),
        "main",
    )
    .unwrap();
    let recovered = dim_agent_capabilities::history::HistoryRenderer::default()
        .context(&main, 7)
        .await
        .unwrap();
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
        project_context_history(
            &dim_agent_capabilities::history::HistoryRenderer::default()
                .context(&child, 2)
                .await
                .unwrap()
        )
        .unwrap(),
        compacted
    );

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
        failure_chain: None,
    });
    let call = EventKind::ModelCall(ModelCallData {
        turn_id: "turn".into(),
        request_id: "request".into(),
        request,
        configuration: None,
        prompt: None,
        failure_chain: None,
    });
    let mut first = record(1, prepare.clone());
    first.batch_end = 2;
    first.trace = Some(TimelineTrace {
        event_id: first.event.id.clone(),
        original: EventDraft::new(prepare.clone()),
        steps: vec![TraceStep {
            handler: "before".into(),
            result: Some(EventDraft {
                id: None,
                tags: first.event.tags.clone(),
                parent_call_id: None,
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
                id: None,
                tags: fifth.event.tags.clone(),
                parent_call_id: None,
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
    assert_eq!(
        timeline_events(&timeline, TimelineQueryScope::Chain, ..)
            .await
            .unwrap()
            .len(),
        10
    );
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

/// A held read snapshot keeps the WAL from checkpointing; once it ends, the
/// writer's checkpoint copies the frames and the size limit truncates the
/// file, so the WAL stays bounded instead of at its high-water mark (#600).
#[tokio::test]
async fn the_wal_file_is_bounded_when_a_read_snapshot_ends() {
    const WAL_LIMIT: u64 = 64 * 1024 * 1024;
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let mut wal = database.0.as_os_str().to_os_string();
    wal.push("-wal");
    let wal_bytes = || std::fs::metadata(&wal).map_or(0, |meta| meta.len());
    let reader = Connection::open(&database.0).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT count(*) FROM timeline_events", [], |row| row.get(0))
        .unwrap();
    let payload = "x".repeat(8_000_000);
    let mut sequence = 0;
    while wal_bytes() <= WAL_LIMIT && sequence < 20 {
        sequence += 1;
        append(
            &store,
            key("session", "main"),
            vec![record(
                sequence,
                EventKind::InputMessage(InputMessageData::text(payload.clone(), None)),
            )],
        )
        .await
        .unwrap();
    }
    assert!(
        wal_bytes() > WAL_LIMIT,
        "a held read snapshot keeps the WAL growing: {} bytes",
        wal_bytes()
    );
    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    for _ in 0..2 {
        sequence += 1;
        append(
            &store,
            key("session", "main"),
            vec![record(
                sequence,
                EventKind::InputMessage(InputMessageData::text(payload.clone(), None)),
            )],
        )
        .await
        .unwrap();
    }
    assert!(
        wal_bytes() <= WAL_LIMIT,
        "the WAL is truncated to its limit after the snapshot ends: {} bytes",
        wal_bytes()
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// Deleting a Session leaves free pages; `dimcode daemon vacuum` rewrites the
/// offline store to reclaim them and refuses a store another process owns.
#[tokio::test]
async fn vacuum_reclaims_a_deleted_session_and_refuses_a_running_store() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    for sequence in 1..=50u64 {
        append(
            &store,
            key("session", "main"),
            vec![record(
                sequence,
                EventKind::InputMessage(InputMessageData::text("x".repeat(40_000), None)),
            )],
        )
        .await
        .unwrap();
    }
    assert!(
        vacuum_store(database.0.clone()).is_err(),
        "a running store owns the database"
    );
    store.rollback_session("session").await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    let before = std::fs::metadata(&database.0).unwrap().len();
    let vacuumed = vacuum_store(database.0.clone()).unwrap();
    assert!(
        vacuumed.after < before,
        "vacuum reclaims free pages: {} >= {}",
        vacuumed.after,
        before
    );
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    let events: i64 = connection
        .query_row("SELECT count(*) FROM timeline_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(events, 0, "vacuum keeps the deleted rows deleted");
    drop(connection);
}

/// A text block's identity is the first delta's event id, so the output-row
/// merge and decode path must preserve that id.
#[tokio::test]
async fn output_block_start_id_roundtrips_through_the_merged_row() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let written = [text_with_id(1, "a", "part-1"), text(2, "b")];
    append(&store, key("session", "main"), written.to_vec())
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session("session".into()).await.unwrap();
    let restored = chain_events(&session, "main", ..=2).await.unwrap();
    let text: Vec<_> = restored
        .iter()
        .filter(|event| matches!(event.kind, EventKind::Text(_)))
        .collect();
    assert_eq!(text.len(), 2);
    assert_eq!(text[0].id, "part-1");
    assert_eq!(text[1].id, "event-2");
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// A producer's preassigned draft id is runtime-only. A before trace must keep
/// the old draft shape so an older reader can still open the store.
#[tokio::test]
async fn stored_trace_drafts_do_not_persist_producer_ids() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let mut traced = text(1, "hello");
    let kind = traced.event.kind.clone();
    traced.trace = Some(TimelineTrace {
        event_id: traced.event.id.clone(),
        original: EventDraft::new(kind.clone()).with_id("block-id"),
        steps: vec![TraceStep {
            handler: "before".into(),
            result: Some(EventDraft::new(kind).with_id("step-id")),
        }],
    });
    append(&store, key("session", "main"), vec![traced])
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let connection = Connection::open(&database.0).unwrap();
    let body: String = connection
        .query_row(
            "SELECT body FROM timeline_events WHERE session_id = ?1 AND sequence = 1",
            ["session"],
            |row| row.get(0),
        )
        .unwrap();
    let row: serde_json::Value = serde_json::from_str(&body).unwrap();
    let trace = &row["records"][0]["trace"];
    assert_eq!(trace["event_id"], "event-1");
    assert!(trace["original"].get("id").is_none());
    assert!(trace["steps"][0]["result"].get("id").is_none());
}

/// An extension event (#2) is stored and read like any other event: it gets a
/// row of its own holding its data's bytes, so the output deltas around it are
/// not merged across it, and a restored Session, whose TimelineTree reads each
/// branch's history once as it opens, decodes it unchanged.
/// A compaction cut may name any event, including one the stream never reads:
/// the stream and the snapshot resolve it by position, not by looking for its
/// row among the flagged ones.
/// A cut on a stored event after the read window is outside the stream.
#[tokio::test]
async fn a_cut_after_the_read_window_fails_the_stream() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    for entry in [
        record(1, EventKind::ContextCompact(ContextCompactData {})),
        record(
            2,
            EventKind::ContextCompaction(completed("event-3", "short", 3)),
        ),
        record(3, EventKind::ContextCompact(ContextCompactData {})),
    ] {
        append(&store, key("session", "main"), vec![entry])
            .await
            .unwrap();
    }
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    let error = HistoryRenderer::default()
        .rendered_stream(&timeline, 2, 256)
        .await
        .expect_err("the cut is after the window");
    assert!(
        error
            .to_string()
            .contains("Compaction cut is outside the stream"),
        "{error}"
    );
    db.close().await.unwrap();
}

/// The cut check starts at the branch's last completed compaction: the store
/// finds its `retain_from` without decoding the prefix, and the fold keeps the
/// verdict of the whole prefix.
#[tokio::test]
async fn a_cut_check_starts_at_the_last_completed_compaction() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let step = |sequence| {
        record(
            sequence,
            EventKind::TurnStart(TurnStartData {
                turn_id: format!("turn-{sequence}"),
            }),
        )
    };
    let events = vec![
        record(
            1,
            EventKind::InputMessage(InputMessageData::text("first", None)),
        ),
        record(
            2,
            EventKind::InputConsumed(InputConsumedData {
                input_id: "event-1".into(),
            }),
        ),
        step(3),
        record(
            4,
            EventKind::ModelStart(ModelStartData {
                turn_id: "turn-3".into(),
                request_id: "request".into(),
            }),
        ),
        text(5, "hello"),
        record(
            6,
            EventKind::ModelOutputEnd(ModelOutputEndData {
                turn_id: "turn-3".into(),
                request_id: "request".into(),
                outcome: ModelOutputOutcome::Completed {
                    finish_reason: FinishReason::EndTurn,
                },
            }),
        ),
        record(
            7,
            EventKind::TurnEnd(TurnEndData {
                turn_id: "turn-3".into(),
                outcome: TurnOutcome::Completed {
                    reason: "end_turn".into(),
                },
            }),
        ),
        step(8),
        record(
            9,
            EventKind::ContextCompaction(completed("event-8", "short", 8)),
        ),
        record(
            10,
            EventKind::ContextCompaction(ContextCompactionData {
                turn_id: Some("turn-8".into()),
                request_id: None,
                command: None,
                start: None,
                through: 9,
                outcome: ContextCompactionResult::Cancelled {
                    reason: "stopped".into(),
                },
            }),
        ),
        record(
            11,
            EventKind::ModelStart(ModelStartData {
                turn_id: "turn-8".into(),
                request_id: "request".into(),
            }),
        ),
        text(12, "after"),
        record(
            13,
            EventKind::ModelOutputEnd(ModelOutputEndData {
                turn_id: "turn-8".into(),
                request_id: "request".into(),
                outcome: ModelOutputOutcome::Completed {
                    finish_reason: FinishReason::EndTurn,
                },
            }),
        ),
        record(
            14,
            EventKind::TurnEnd(TurnEndData {
                turn_id: "turn-8".into(),
                outcome: TurnOutcome::Completed {
                    reason: "end_turn".into(),
                },
            }),
        ),
    ];
    append(&store, key("session", "main"), events)
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
    for at in [4, 8, 12, 14] {
        let expected = dim_agent_capabilities::history::validate_context_boundary(
            &dim_agent_capabilities::history::HistoryRenderer::default()
                .replay_context(&timeline, at)
                .await
                .unwrap(),
        );
        assert_eq!(
            dim_agent_capabilities::history::validate_context_boundary_at(&timeline, at)
                .await
                .unwrap(),
            expected,
            "cut {at}"
        );
    }
    db.close().await.unwrap();
}

/// A completed compaction on a branch whose cut names an ancestor's event is
/// not a resume point for that branch: the check replays the branch's segment
/// and keeps the whole-prefix verdict.
#[tokio::test]
async fn a_cut_on_another_branch_replays_the_segment() {
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
                EventKind::TurnStart(TurnStartData {
                    turn_id: "turn-main".into(),
                }),
            ),
            record(
                2,
                EventKind::ModelStart(ModelStartData {
                    turn_id: "turn-main".into(),
                    request_id: "request-main".into(),
                }),
            ),
        ],
    )
    .await
    .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute_batch("INSERT INTO timeline_branches VALUES ('session', 'child', '{}', 0); INSERT INTO timeline_member_roots SELECT session_id,'child',index_id,group_key,0,NULL FROM timeline_member_roots WHERE session_id='session' AND branch_id='main' AND sequence=0;")
        .unwrap();
    drop(connection);
    append(
        &store,
        key("session", "child"),
        vec![
            record(
                1,
                EventKind::BranchStart(BranchStartData {
                    parent: Some("main".into()),
                    create_event: None,
                }),
            ),
            record(
                2,
                EventKind::TurnStart(TurnStartData {
                    turn_id: "turn-child".into(),
                }),
            ),
            record(
                3,
                EventKind::ModelStart(ModelStartData {
                    turn_id: "turn-child".into(),
                    request_id: "request-child".into(),
                }),
            ),
            record(
                4,
                EventKind::ContextCompaction(completed("event-1", "short", 4)),
            ),
        ],
    )
    .await
    .unwrap();
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "child"),
        vec![("main".into(), 2)],
        store.clone(),
    )
    .await
    .unwrap();
    for at in [3, 4] {
        let expected = dim_agent_capabilities::history::validate_context_boundary(
            &dim_agent_capabilities::history::HistoryRenderer::default()
                .replay_context(&timeline, at)
                .await
                .unwrap(),
        );
        assert!(expected.is_err(), "the output is open at {at}");
        assert_eq!(
            dim_agent_capabilities::history::validate_context_boundary_at(&timeline, at)
                .await
                .unwrap(),
            expected,
            "cut {at}"
        );
    }
    db.close().await.unwrap();
}

/// A cut on an ancestor event after that ancestor's fork cut is outside the
/// stream too.
#[tokio::test]
async fn a_cut_beyond_the_ancestor_cut_fails_the_stream() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    for entry in
        (1..=5).map(|sequence| record(sequence, EventKind::ContextCompact(ContextCompactData {})))
    {
        append(&store, key("session", "main"), vec![entry])
            .await
            .unwrap();
    }
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute_batch("INSERT INTO timeline_branches VALUES ('session', 'child', '{}', 0); INSERT INTO timeline_member_roots SELECT session_id,'child',index_id,group_key,0,NULL FROM timeline_member_roots WHERE session_id='session' AND branch_id='main' AND sequence=0;")
        .unwrap();
    drop(connection);
    append(
        &store,
        key("session", "child"),
        vec![record(
            1,
            EventKind::ContextCompaction(completed("event-5", "short", 1)),
        )],
    )
    .await
    .unwrap();
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "child"),
        vec![("main".into(), 2)],
        store.clone(),
    )
    .await
    .unwrap();
    let error = HistoryRenderer::default()
        .rendered_stream(&timeline, 1, 256)
        .await
        .expect_err("the cut is beyond the ancestor's cut");
    assert!(
        error
            .to_string()
            .contains("Compaction cut is outside the stream"),
        "{error}"
    );
    db.close().await.unwrap();
}

/// A later compaction whose cut precedes the one in effect is damaged data:
/// the stream reports it instead of draining nothing.
#[tokio::test]
async fn a_cut_that_precedes_the_cut_in_effect_fails_the_stream() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    for entry in [
        record(1, EventKind::ContextCompact(ContextCompactData {})),
        record(
            2,
            EventKind::ContextCompaction(completed("event-3", "first", 3)),
        ),
        record(3, EventKind::ContextCompact(ContextCompactData {})),
        record(
            4,
            EventKind::ContextCompaction(completed("event-1", "second", 4)),
        ),
    ] {
        append(&store, key("session", "main"), vec![entry])
            .await
            .unwrap();
    }
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    let error = HistoryRenderer::default()
        .rendered_stream(&timeline, 4, 256)
        .await
        .expect_err("the second cut is earlier than the first");
    assert!(
        error
            .to_string()
            .contains("Compaction cut precedes the cut in effect"),
        "{error}"
    );
    db.close().await.unwrap();
}

#[tokio::test]
async fn a_cut_on_an_unflagged_event_still_streams_and_saves_its_checkpoint() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    for entry in [
        record(1, EventKind::ContextCompact(ContextCompactData {})),
        record(
            2,
            EventKind::ContextCompaction(completed("event-1", "short", 2)),
        ),
    ] {
        append(&store, key("session", "main"), vec![entry])
            .await
            .unwrap();
    }
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        EventKind::ContextCompact(ContextCompactData {}).context_flags(),
        (false, false)
    );
    let renderer = HistoryRenderer::default();
    renderer.rendered_stream(&timeline, 2, 256).await.unwrap();
    let checkpoint = renderer
        .stream_checkpoint(&timeline, 2, "event-1", 256)
        .await
        .unwrap();
    assert_eq!(
        (
            checkpoint.resume.branch_id.as_str(),
            checkpoint.resume.sequence
        ),
        ("main", 1)
    );
    assert_eq!(checkpoint.state.retain_from, "event-1");
    db.close().await.unwrap();
}

#[tokio::test]
async fn extension_events_store_the_switches_their_producer_sets() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let marked = ExtensionEvent::new("myplugin", "state", serde_json::json!({"open": true}))
        .unwrap()
        .with_context_flags(true, true);
    let unmarked = ExtensionEvent::new("myplugin", "noise", serde_json::json!({})).unwrap();
    append(
        &store,
        key("session", "main"),
        vec![
            record(1, EventKind::Extension(marked)),
            record(2, EventKind::Extension(unmarked)),
        ],
    )
    .await
    .unwrap();
    let scope = dim_agent_runtime::TimelineReadScope {
        key: key("session", "main"),
        through: 2,
        segments: vec![dim_agent_runtime::TimelineReadSegment {
            branch_id: "main".into(),
            from: 1,
            through: 2,
            stored_through: 2,
            tail: vec![],
        }],
    };
    let mut request = position_queries::query(TimelineQueryScope::Branch, 1);
    request.query.filter = Some(dim_agent_runtime::QueryFilter::Or {
        conditions: ["context.render", "context.state"]
            .into_iter()
            .map(|path| position_queries::eq(path, true.into()))
            .collect(),
    });
    let read = store
        .query(&scope, &request.query, request.page.as_ref())
        .await
        .unwrap();
    let dim_agent_runtime::QueryOutput::Events { events } = read.result else {
        panic!("events")
    };
    assert_eq!(
        events
            .iter()
            .map(|row| row.event.sequence)
            .collect::<Vec<_>>(),
        vec![1],
        "only the event the producer marked is read for the context"
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    let rows = connection
        .prepare("SELECT context_render, context_state FROM timeline_events ORDER BY sequence")
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows, vec![(1, 1), (0, 0)]);
}

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
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session("session".into()).await.unwrap();
    let restored = chain_events(&session, "main", ..=3).await.unwrap();
    let written: Vec<_> = written.iter().map(|entry| entry.event.clone()).collect();
    assert_eq!(
        serde_json::to_string(&restored).unwrap(),
        serde_json::to_string(&written).unwrap()
    );
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn branch_end_events_restore_unchanged_and_the_line_stays_ended() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let ended = EventKind::BranchEnded(BranchEndedData {
        branch: "child".into(),
        end_event: 2,
        source: BranchSource::Host {
            operation_id: "session.branch_end".into(),
        },
        ancestor: None,
    });
    append(
        &store,
        key("session", "main"),
        vec![
            text(1, "a"),
            record(
                2,
                EventKind::BranchCreate(BranchCreateData {
                    at: BranchCreateAt::Create {},
                    source: BranchSource::Tool {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        call_id: "call".into(),
                    },
                    branch: "child".into(),
                    prompt: "work".into(),
                }),
            ),
            text(3, "b"),
            record(4, ended.clone()),
            text(5, "c"),
        ],
    )
    .await
    .unwrap();
    let end = EventKind::BranchEnd(BranchEndData {
        source: BranchSource::Tool {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "call".into(),
        },
        ancestor: None,
    });
    append(
        &store,
        key("session", "child"),
        vec![
            record(
                1,
                EventKind::BranchStart(BranchStartData {
                    parent: Some("main".into()),
                    create_event: Some(2),
                }),
            ),
            record(2, end.clone()),
        ]
        .into_iter()
        .map(|record| StoredEvent {
            event: Arc::new(AgentEvent {
                id: format!("child-{}", record.event.id),
                ..(*record.event).clone()
            }),
            ..record
        })
        .collect(),
    )
    .await
    .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let connection = Connection::open(&database.0).unwrap();
    let rows = connection
        .prepare(
            "SELECT branch_id, sequence, last_sequence, kind FROM timeline_events ORDER BY branch_id, sequence",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|(branch, sequence, last, kind)| {
                (branch.as_str(), *sequence, *last, kind.as_str())
            })
            .collect::<Vec<_>>(),
        [
            ("child", 1, 1, "branch/start"),
            ("child", 2, 2, "branch/end"),
            ("main", 1, 1, "step/text"),
            ("main", 2, 2, "branch/create"),
            ("main", 3, 3, "step/text"),
            ("main", 4, 4, "branch/ended"),
            ("main", 5, 5, "step/text"),
        ]
    );
    let info: String = connection
        .query_row(
            "SELECT info FROM timeline_branches WHERE session_id = 'session' AND branch_id = 'child'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        info,
        r#"{"id":"child","parent":"main","branch_point":2,"create_anchor":2}"#
    );
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session("session".into()).await.unwrap();
    let restored = chain_events(&session, "main", ..=5).await.unwrap();
    assert!(
        restored
            .iter()
            .any(|event| event.sequence == 4 && event.kind == ended)
    );
    assert!(!session.branches().iter().any(|line| line.id == "child"));
    assert_eq!(session.sequence("child").unwrap(), 2);
    let line = store
        .read_page(&key("session", "child"), 0, 8, 2)
        .await
        .unwrap()
        .into_iter()
        .map(|record| record.event)
        .collect::<Vec<_>>();
    assert_eq!(line.len(), 2);
    assert_eq!(line[1].kind, end);
    assert!(matches!(
        session
            .submit("late".into(), InputMode::Queue, "child")
            .await,
        Err(AgentError::BranchEnded(_))
    ));
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
            failure_chain: None,
        })
    };
    let call = |request: &Arc<ModelRequest>| {
        EventKind::ModelCall(ModelCallData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            request: request.clone(),
            configuration: None,
            prompt: None,
            failure_chain: None,
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
    let second = request(
        vec![text(user, "u1"), text(assistant, "a1"), text(user, "u2")],
        Some("one"),
        Some(&["echo"]),
    );
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
            id: None,
            tags: traced.event.tags.clone(),
            parent_call_id: None,
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
            failure_chain: None,
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
    let checkpoint = projection_checkpoint(
        &dim_agent_capabilities::history::HistoryRenderer::default()
            .replay_context(&timeline, through)
            .await
            .unwrap(),
        &retain_from,
    )
    .unwrap();
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
        store.branches("long").await.unwrap();
        let validation = store.take_node_loads();
        assert_eq!(validation.loads, 0, "validation decodes no request body");
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
        let from_resume = store.read(&main, resume..=head).await.unwrap();
        assert_eq!(from_resume.len() as u64, head - resume + 1);
        let loads = store.take_node_loads();
        assert_eq!(loads.loads, nodes, "budget {budget:?}");
        assert!(loads.max_per_hold <= 256);
        store.close().await.unwrap();
    }
    let store = SqliteTimelineStore::new(db.clone());
    let context = || async {
        let timeline = Timeline::restore(
            rutis::Ctx::root().unwrap(),
            main.clone(),
            vec![],
            store.clone(),
        )
        .await
        .unwrap();
        let context = dim_agent_capabilities::history::HistoryRenderer::default()
            .context(&timeline, head)
            .await
            .unwrap();
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
            "UPDATE timeline_events SET body = '{}' WHERE session_id = 'long' AND last_sequence < ?1",
            [resume as i64],
        )
        .unwrap();
    assert_eq!(overwritten as u64, resume - 1);
    store.branches("long").await.unwrap();
    store.take_node_loads();
    let after = context().await;
    assert_eq!(after, before);
    assert!(store.take_node_loads().max_per_hold <= 256);
    drop(connection);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn paged_reads_decode_requested_events_and_check_request_content() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("paging", &SessionConfig::default())
        .await
        .unwrap();
    let main = key("paging", "main");
    let messages: Vec<Message> = (1..=6)
        .map(|index| Message::text(Role::User, format!("message-{index}")))
        .collect();
    append(
        &store,
        main.clone(),
        vec![
            record(
                1,
                EventKind::InputMessage(InputMessageData::text("go", Some(InputMode::Queue))),
            ),
            model_call(2, messages.clone()),
            text(3, "done"),
        ],
    )
    .await
    .unwrap();
    store.take_node_loads();

    let rebuilt = store.read_page(&main, 0, 8, 3).await.unwrap();
    assert_eq!(rebuilt[1].event, model_call(2, messages).event);
    assert_eq!(store.take_node_loads().loads, 6);

    let connection = Connection::open(&database.0).unwrap();
    let damaged = connection
        .execute(
            "UPDATE timeline_content SET body = replace(body, 'message-1', 'message-X') \
             WHERE session_id = 'paging' AND kind = 'node'",
            [],
        )
        .unwrap();
    assert!(damaged > 0, "the request's nodes are stored");
    assert!(
        matches!(
            store.read_page(&main, 0, 8, 3).await,
            Err(AgentError::Storage(message)) if message.contains("request content hash mismatch")
        ),
        "a read that uses the requests checks their content"
    );
    drop(connection);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// One page of `branch`'s events through the timeline store.
async fn page(
    store: &Arc<dyn TimelineStore>,
    id: &str,
    branch: &str,
    after: u64,
    limit: usize,
    through: u64,
) -> Vec<Arc<dim_agent_types::AgentEvent>> {
    store
        .read_page(&key(id, branch), after, limit, through)
        .await
        .unwrap()
        .into_iter()
        .map(|record| record.event)
        .collect()
}

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
            if rewrite {
                holds(2, fresh)
            } else {
                NodeLoads {
                    encoded: fresh,
                    ..Default::default()
                }
            },
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
            if rewrite {
                holds(4, 2 * fresh)
            } else {
                NodeLoads {
                    encoded: 2 * fresh,
                    ..Default::default()
                }
            },
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

#[tokio::test]
async fn cold_request_writes_keep_equal_values_with_different_json_order() {
    let message = |input: &str| Message {
        role: Role::Assistant,
        content: MessageContent::Parts(vec![MessagePart::ToolUse {
            provider_metadata: None,
            id: "call".into(),
            name: "echo".into(),
            input: serde_json::from_str(input).unwrap(),
            start_time: None,
        }]),
    };
    let first = message(r#"{"a":"one","b":"two"}"#);
    let reordered = message(r#"{"b":"two","a":"one"}"#);
    assert_eq!(first, reordered);
    assert_ne!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&reordered).unwrap()
    );
    let records = [first, reordered]
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let mut messages = vec![message];
            if index > 0 {
                messages.push(Message::text(Role::User, "next input"));
            }
            model_call(index as u64 + 1, messages)
        })
        .collect::<Vec<_>>();
    let mut written = Vec::new();
    for memo in [true, false] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = if memo {
            SqliteTimelineStore::new(db.clone())
        } else {
            SqliteTimelineStore::without_writer_memo(db.clone())
        };
        store
            .create_session("ordered", &SessionConfig::default())
            .await
            .unwrap();
        for record in &records {
            append(&store, key("ordered", "main"), vec![record.clone()])
                .await
                .unwrap();
        }
        assert_eq!(
            store.take_node_loads().loads,
            usize::from(!memo),
            "unequal addresses still need the old value comparison"
        );
        let actual = store.read(&key("ordered", "main"), 1..=2).await.unwrap();
        assert_eq!(
            actual.iter().map(|row| &row.event).collect::<Vec<_>>(),
            records.iter().map(|row| &row.event).collect::<Vec<_>>()
        );
        let connection = Connection::open(&database.0).unwrap();
        written.push(session_rows(&connection, "ordered"));
        drop(connection);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    assert_eq!(
        written[0], written[1],
        "writer liveness cannot change the persisted encoding"
    );
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
    let history = request_history();
    let request = record(1, history[1].1.event.kind.clone());
    append(&store, key("bad", "main"), vec![request])
        .await
        .unwrap();
    assert_eq!(store.writer_memo_requests("bad"), 1);
    let connection = Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_bad BEFORE INSERT ON timeline_events WHEN NEW.session_id = 'bad' BEGIN SELECT RAISE(ABORT, 'injected failure'); END").unwrap();
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
    let (unexpected, rejected_callback) = tokio::sync::oneshot::channel();
    assert!(
        store
            .append(
                key("bad", "main"),
                vec![text(2, "late")],
                Box::new(move |_| {
                    let _ = unexpected.send(());
                })
            )
            .is_err()
    );
    assert!(
        rejected_callback.await.is_err(),
        "rejected append callback must not run"
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
    for reopen in [false, true] {
        let database = Database::new();
        let mut db = SqliteStorage::open(database.0.clone()).unwrap();
        let mut store = SqliteTimelineStore::new(db.clone());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::mock_hosts(&mut builder);
        dim_agent_capabilities::defaults::install(&mut builder);
        builder.timeline_store(store.clone());
        let mut agent = MinimalHost::start(&builder).await.unwrap();
        let bad = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let mut good = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let bad_id = bad.id().to_owned();
        let connection = Connection::open(&database.0).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_output BEFORE INSERT ON timeline_events WHEN NEW.kind = 'step/text' AND NEW.session_id = (SELECT session_id FROM injected_session) BEGIN SELECT RAISE(ABORT, 'injected output commit failure'); END; CREATE TABLE injected_session(session_id TEXT)").unwrap();
        connection
            .execute("INSERT INTO injected_session VALUES (?1)", [&bad_id])
            .unwrap();
        let (admitted_bad, admitted_good) = tokio::join!(
            bad.submit("failed output".into(), InputMode::Queue, "main"),
            good.submit("healthy output".into(), InputMode::Queue, "main"),
        );
        admitted_bad.unwrap();
        admitted_good.unwrap();
        let error = settled("the failed Session closes", bad.closed())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("injected output commit failure"));
        settled("the healthy branch idles", good.idle("main"))
            .await
            .unwrap();
        settled("the idle request body is released", async {
            while store.writer_memo_requests(good.id()) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(store.writer_memo_requests(&bad_id), 0);
        assert!(agent.get_session(&bad_id).is_none());
        assert!(agent.get_session(good.id()).is_some());
        let bad_history = chain_events(&bad, "main", ..).await.unwrap();
        assert!(
            bad_history
                .iter()
                .any(|event| matches!(event.kind, EventKind::ModelCall(_)))
        );
        assert!(!bad_history.iter().any(|event| matches!(
            event.kind,
            EventKind::Text(_) | EventKind::ModelOutputEnd(_) | EventKind::TurnEnd(_)
        )));
        assert!(
            bad.submit("late".into(), InputMode::Queue, "main")
                .await
                .unwrap_err()
                .to_string()
                .contains("injected output commit failure")
        );
        assert!(chain_events(&good, "main", ..).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("healthy output"))));
        connection
            .execute_batch("DROP TRIGGER fail_output; DROP TABLE injected_session")
            .unwrap();
        connection.execute_batch("CREATE TRIGGER fail_restore BEFORE INSERT ON timeline_events WHEN NEW.session_id = (SELECT session_id FROM restore_failure) BEGIN SELECT RAISE(ABORT, 'injected restore settlement failure'); END; CREATE TABLE restore_failure(session_id TEXT)").unwrap();
        connection
            .execute("INSERT INTO restore_failure VALUES (?1)", [&bad_id])
            .unwrap();
        let head = store.head(&key(&bad_id, "main")).await.unwrap();
        let failed_restore = settled(
            "a failed settlement rejects restore",
            agent.restore_session(bad_id.clone()),
        )
        .await
        .err()
        .expect("a failed settlement must reject restore");
        assert!(
            failed_restore
                .to_string()
                .contains("injected restore settlement failure"),
            "{failed_restore}"
        );
        assert!(agent.get_session(&bad_id).is_none());
        assert_eq!(store.head(&key(&bad_id, "main")).await.unwrap(), head);
        connection
            .execute_batch("DROP TRIGGER fail_restore; DROP TABLE restore_failure")
            .unwrap();
        drop(connection);
        if reopen {
            let good_id = good.id().to_owned();
            agent.close().await.unwrap();
            store.close().await.unwrap();
            db.close().await.unwrap();
            db = SqliteStorage::open(database.0.clone()).unwrap();
            store = SqliteTimelineStore::new(db.clone());
            let mut builder = AgentBuilder::new(AgentConfig::default());
            dim_agent_capabilities::defaults::mock_hosts(&mut builder);
            dim_agent_capabilities::defaults::install(&mut builder);
            builder.timeline_store(store.clone());
            agent = MinimalHost::start(&builder).await.unwrap();
            good = agent.restore_session(good_id).await.unwrap();
        }
        let session = settled("the orphan restores", agent.restore_session(bad_id))
            .await
            .unwrap();
        assert!(
            bad.submit("old generation".into(), InputMode::Queue, "main")
                .await
                .is_err()
        );
        good.submit("still healthy".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        settled("the healthy branch idles after recovery", good.idle("main"))
            .await
            .unwrap();
        assert!(chain_events(&good, "main", ..).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("still healthy"))));
        settled(
            "the restored branch settles the orphaned turn",
            session.idle("main"),
        )
        .await
        .unwrap();
        let history = chain_events(&session, "main", ..).await.unwrap();
        assert!(history.iter().any(|event| matches!(&event.kind, EventKind::ModelOutputEnd(data) if matches!(data.outcome, ModelOutputOutcome::Failed { .. }))));
        assert!(history.iter().any(|event| matches!(&event.kind, EventKind::TurnEnd(data) if matches!(data.outcome, TurnOutcome::Failed { .. }))));
        session
            .submit("after recovery".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        settled("the restored branch ends its turn", session.idle("main"))
            .await
            .unwrap();
        assert!(chain_events(&session, "main", ..).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("after recovery"))));
        settled(
            "the recovered Session releases its completed request",
            async {
                while store.writer_memo_requests(session.id()) != 0 {
                    tokio::task::yield_now().await;
                }
            },
        )
        .await;
        assert!(session.unload_if_idle().await.unwrap());
        settled("the unloaded Session closes", session.closed())
            .await
            .unwrap();
        let id = session.id().to_owned();
        drop(session);
        assert_eq!(store.writer_memo_requests(&id), 0);
        agent.close().await.unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
}

#[tokio::test]
async fn selected_reads_reject_corrupt_records_and_preserve_them() {
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
    store.branches("session").await.unwrap();
    assert!(
        store
            .read_page(&key("session", "main"), 0, 1, 1)
            .await
            .is_err()
    );
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
    store.branches("session").await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    assert_eq!(
        connection
            .query_row("SELECT branch_id FROM timeline_events", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "missing"
    );

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
                failure_chain: None,
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
                failure_chain: None,
            }),
        )
    };
    let messages = |prefix: &str, count: usize| -> Vec<Message> {
        (0..count)
            .map(|index| Message::text(Role::User, format!("{prefix} {index}")))
            .collect()
    };
    let short = request(messages("m", 3));
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
        let checking = SqliteTimelineStore::new(db.clone());
        checking.branches("lists").await.unwrap();
        checking.close().await.unwrap();
        assert_eq!(connection.execute(&set, [&intact, &damaged]).unwrap(), 1);
    }
    store.branches("lists").await.unwrap();

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
    store.branches("long").await.unwrap();
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
async fn directory_reads_do_not_load_request_content() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let request = || {
        let mut request = ModelRequest::new(
            "mock",
            "test",
            (0..3)
                .map(|index| Message::text(Role::User, format!("m {index}")))
                .collect(),
        );
        request.system = Some(Arc::new(SystemPrompt::Text("system".into())));
        Arc::new(request)
    };
    for session in ["corrupt", "missing"] {
        let prepare = record(
            1,
            EventKind::ContextPrepare(ContextPrepareData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                through: 0,
                request: request(),
                configuration: None,
                prompt: None,
                recovery: None,
                failure_chain: None,
            }),
        );
        let call = record(
            2,
            EventKind::ModelCall(ModelCallData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                request: request(),
                configuration: None,
                prompt: None,
                failure_chain: None,
            }),
        );
        store
            .create_session(session, &SessionConfig::default())
            .await
            .unwrap();
        append(&store, key(session, "main"), vec![prepare, call])
            .await
            .unwrap();
    }
    let connection = Connection::open(&database.0).unwrap();
    let system = |session: &str| -> String {
        connection
            .query_row(
                "SELECT hash FROM timeline_content WHERE session_id = ?1 AND kind = 'system'",
                [session],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        connection
            .execute(
                "UPDATE timeline_content SET body = 'not the prompt' \
                 WHERE session_id = 'corrupt' AND kind = 'system'",
                [],
            )
            .unwrap(),
        1
    );
    store.branches("corrupt").await.unwrap();
    assert!(matches!(
        store.read(&key("corrupt", "main"), 1..=2).await,
        Err(AgentError::Storage(message)) if message.contains("request content hash mismatch")
    ));
    let missing = system("missing");
    assert_eq!(
        connection
            .execute(
                "DELETE FROM timeline_content WHERE session_id = 'missing' AND kind = 'system'",
                [],
            )
            .unwrap(),
        1
    );
    store.branches("missing").await.unwrap();
    assert!(matches!(store.read(&key("missing","main"),1..=2).await,
        Err(AgentError::Storage(message)) if message == format!("missing request content {missing}")));

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
        V19,
        V20,
        V21,
        V22,
        V23,
        V24,
        Current,
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
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 6));
            INSERT INTO agent_store_format VALUES (6);"#,
        ),
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 7));
            INSERT INTO agent_store_format VALUES (7);"#,
        ),
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 8));
            INSERT INTO agent_store_format VALUES (8);"#,
        ),
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 10));
            INSERT INTO agent_store_format VALUES (10);"#,
        ),
        (
            Base::V13,
            r#"DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 11));
            INSERT INTO agent_store_format VALUES (11);
            DROP TABLE timeline_snapshots;
            ALTER TABLE timeline_events ADD COLUMN checkpoint INTEGER NOT NULL DEFAULT 0;
            CREATE INDEX timeline_events_checkpoints ON timeline_events(session_id,branch_id,checkpoint,sequence);"#,
        ),
        (
            Base::V13,
            r#"DROP TABLE provider_imports; DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 9));
            INSERT INTO agent_store_format VALUES (9);
            INSERT INTO providers VALUES ('preserve',7,'{"credential":"original"}');"#,
        ),
        (
            Base::V13,
            r#"DROP TABLE settings_documents; DROP TABLE agent_store_format;
            CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 14));
            INSERT INTO agent_store_format VALUES (14);
            CREATE TABLE user_configuration (layer TEXT NOT NULL PRIMARY KEY CHECK(layer = 'user'), version INTEGER NOT NULL CHECK(version > 0), envelope TEXT NOT NULL);
            INSERT INTO user_configuration VALUES ('user', 7, '{"format":1,"patches":[],"documents":{},"imports":{}}');"#,
        ),
        (
            Base::V13,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=14",
        ),
        (Base::V13, "DELETE FROM agent_store_format"),
        (
            Base::V13,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=99",
        ),
        (Base::V15, "DELETE FROM agent_store_format"),
        (
            Base::V15,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=13",
        ),
        (
            Base::V15,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=16",
        ),
        (Base::V16, "DELETE FROM agent_store_format"),
        (
            Base::V16,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=15",
        ),
        (
            Base::V16,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=17",
        ),
        (Base::V17, "DELETE FROM agent_store_format"),
        (
            Base::V17,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=16",
        ),
        (
            Base::V17,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=18",
        ),
        (Base::V18, "DELETE FROM agent_store_format"),
        (
            Base::V18,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=17",
        ),
        (
            Base::V18,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=19",
        ),
        (Base::V19, "DELETE FROM agent_store_format"),
        (
            Base::V19,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=18",
        ),
        (
            Base::V19,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=20",
        ),
        (Base::V20, "DELETE FROM agent_store_format"),
        (
            Base::V20,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=19",
        ),
        (
            Base::V20,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=21",
        ),
        (Base::V21, "DELETE FROM agent_store_format"),
        (
            Base::V21,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=22",
        ),
        (Base::V22, "DELETE FROM agent_store_format"),
        (
            Base::V22,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=21",
        ),
        (
            Base::V23,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=24",
        ),
        (Base::V24, "DELETE FROM agent_store_format"),
        (Base::V24, "DROP INDEX timeline_events_by_end"),
        (Base::Current, "DELETE FROM agent_store_format"),
        (
            Base::Current,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=24",
        ),
        (
            Base::Current,
            "CREATE INDEX timeline_events_by_end ON timeline_events(session_id, branch_id, last_sequence, sequence)",
        ),
        (
            Base::V24,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=23",
        ),
        (
            Base::V21,
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=20",
        ),
        (
            Base::V13,
            "CREATE TABLE session_files (file_id TEXT NOT NULL PRIMARY KEY, node TEXT, writer TEXT)",
        ),
        (Base::V15, "DROP INDEX session_file_owners_by_file"),
        (Base::V15, V16_ADDITIONS[0]),
        (Base::V16, "DROP TABLE session_metadata"),
        (Base::V17, "DROP TABLE session_metadata"),
        (Base::V18, "DROP TABLE session_metadata"),
        (Base::V19, "DROP TABLE session_metadata"),
        (Base::V20, "DROP TABLE session_metadata"),
        (Base::V21, "DROP TABLE session_metadata"),
        (Base::V22, "DROP TABLE session_metadata"),
        (Base::V23, "DROP TABLE session_metadata"),
        (Base::V24, "DROP TABLE session_metadata"),
        (Base::Current, "DROP TABLE session_metadata"),
    ] {
        let database = Database::new();
        match base {
            Base::Empty => {}
            Base::V13 => drop(v13_store(&database.0)),
            Base::V15 => drop(v15_store(&database.0)),
            Base::V16 => drop(v16_tables_store(&database.0, 16)),
            Base::V17 => drop(v16_tables_store(&database.0, 17)),
            Base::V18 => drop(v16_tables_store(&database.0, 18)),
            Base::V19 => drop(v16_tables_store(&database.0, 19)),
            Base::V20 => drop(v16_tables_store(&database.0, 20)),
            Base::V21 => drop(v16_tables_store(&database.0, 21)),
            Base::V22 => drop(v16_tables_store(&database.0, 22)),
            Base::V23 => drop(catalog_store(&database.0, 23)),
            Base::V24 => drop(catalog_store(&database.0, 24)),
            Base::Current => drop(SqliteStorage::open(database.0.clone()).unwrap()),
        }
        let connection = Connection::open(&database.0).unwrap();
        connection.execute_batch(sql).unwrap();
        drop(connection);
        let original = std::fs::read(&database.0).unwrap();
        for _ in 0..2 {
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

fn startup_note(database: &std::path::Path) -> PathBuf {
    let mut note = database.as_os_str().to_os_string();
    note.push(".startup");
    note.into()
}

fn read_startup_note(database: &std::path::Path) -> String {
    let note = std::fs::read_to_string(startup_note(database)).unwrap();
    let (header, body) = note.split_once('\n').unwrap();
    assert_eq!(
        header,
        format!("dimcode-startup-failure v1 pid={}", std::process::id())
    );
    assert!(!body.is_empty(), "{note}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(startup_note(database))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{note}");
    }
    body.to_owned()
}

/// #1335: a store another process owns leaves a startup note that says so,
/// and names the owner and the formats when the owner's progress note does.
#[test]
fn an_owned_store_leaves_a_note_that_names_the_owner() {
    let database = Database::new();
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(format!("{}.lock", database.0.display()))
        .unwrap();
    lease.lock().unwrap();
    let Err(AgentError::Storage(message)) = SqliteStorage::open(database.0.clone()) else {
        panic!("an owned store must be refused");
    };
    assert!(
        message.contains("another process owns the SQLite database"),
        "{message}"
    );
    let note = read_startup_note(&database.0);
    assert!(
        note.contains("Another dimcode-server is using the store."),
        "{note}"
    );
    assert!(
        note.contains("Wait for it to finish, then run the command again."),
        "{note}"
    );
    // A running upgrade reports itself beside the store; the refusal then
    // names the process and the formats to wait for.
    std::fs::write(
        startup_note(&database.0),
        format!(
            "dimcode-startup-progress v1 pid={}\nstore-upgrade from=34 to=36 progress=7\n",
            std::process::id()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            startup_note(&database.0),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let Err(AgentError::Storage(message)) = SqliteStorage::open(database.0.clone()) else {
        panic!("an owned store must be refused");
    };
    assert!(
        message.contains("another process owns the SQLite database"),
        "{message}"
    );
    let note = read_startup_note(&database.0);
    assert!(
        note.contains(&format!(
            "Another dimcode-server (pid {}) is upgrading it from format v34 to v36.",
            std::process::id()
        )),
        "{note}"
    );
    lease.unlock().unwrap();
}

#[test]
fn a_newer_store_leaves_an_actionable_startup_note() {
    let database = Database::new();
    let future = STORE_FORMAT + 1;
    Connection::open(&database.0)
        .unwrap()
        .execute_batch(
            &format!("CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = {future}));
             INSERT INTO agent_store_format VALUES ({future});"),
        )
        .unwrap();
    let original = std::fs::read(&database.0).unwrap();
    let Err(AgentError::Storage(message)) = SqliteStorage::open(database.0.clone()) else {
        panic!("a newer store must be rejected");
    };
    assert!(message.contains("unsupported SQLite schema"), "{message}");
    assert_eq!(std::fs::read(&database.0).unwrap(), original);
    let note = read_startup_note(&database.0);
    assert!(note.contains(&format!("Store format: v{future}")), "{note}");
    assert!(
        note.contains(&format!("Daemon format: v{STORE_FORMAT}")),
        "{note}"
    );
    assert!(note.contains("Existing data was preserved"), "{note}");
    assert!(
        note.contains(&format!("Run a build supporting v{future}")),
        "{note}"
    );
    assert!(note.contains("--home PATH"), "{note}");
}

#[test]
fn a_mismatched_store_note_does_not_claim_a_downgrade() {
    let database = Database::new();
    drop(SqliteStorage::open(database.0.clone()).unwrap());
    Connection::open(&database.0)
        .unwrap()
        .execute_batch(
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=29",
        )
        .unwrap();
    let original = std::fs::read(&database.0).unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    assert_eq!(std::fs::read(&database.0).unwrap(), original);
    let note = read_startup_note(&database.0);
    assert!(
        note.contains(&format!("Store format: v29 (schema is v{STORE_FORMAT})")),
        "{note}"
    );
    assert!(note.contains("marker and schema disagree"), "{note}");
    assert!(
        note.contains("Use the build that wrote this store"),
        "{note}"
    );
    assert!(!note.contains("Run a build supporting"), "{note}");
}

#[test]
fn an_older_known_schema_with_a_newer_marker_is_a_mismatch() {
    let database = Database::new();
    let connection = v13_store(&database.0);
    connection
        .execute_batch(
            "PRAGMA ignore_check_constraints=ON; UPDATE agent_store_format SET version=32",
        )
        .unwrap();
    drop(connection);
    let original = std::fs::read(&database.0).unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    assert_eq!(std::fs::read(&database.0).unwrap(), original);
    let note = read_startup_note(&database.0);
    assert!(note.contains("Store format: v32 (schema is v13)"), "{note}");
    assert!(
        note.contains("Use the build that wrote this store"),
        "{note}"
    );
    assert!(!note.contains("Run a build supporting"), "{note}");
}

#[test]
fn an_empty_marker_table_is_reported_as_missing_or_ambiguous() {
    let database = Database::new();
    drop(SqliteStorage::open(database.0.clone()).unwrap());
    Connection::open(&database.0)
        .unwrap()
        .execute_batch("DELETE FROM agent_store_format")
        .unwrap();
    let original = std::fs::read(&database.0).unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    assert_eq!(std::fs::read(&database.0).unwrap(), original);
    let note = read_startup_note(&database.0);
    assert!(
        note.contains(&format!(
            "Store format: v{STORE_FORMAT} (format marker is missing or ambiguous)"
        )),
        "{note}"
    );
    assert!(!note.contains("Run a build supporting"), "{note}");
}

#[test]
fn an_unrecognized_store_with_a_readable_marker_names_it() {
    let database = Database::new();
    Connection::open(&database.0)
        .unwrap()
        .execute_batch(
            "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY);
             INSERT INTO agent_store_format VALUES (20);
             CREATE TABLE unrelated (value INTEGER);",
        )
        .unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    let note = read_startup_note(&database.0);
    assert!(
        note.contains("Store format: v20 (unrecognized schema)"),
        "{note}"
    );
    assert!(
        note.contains("The store is not a format this daemon recognizes"),
        "{note}"
    );
    assert!(!note.contains("Run a build supporting"), "{note}");
}

#[test]
fn an_unrecognized_store_note_names_no_guessed_format() {
    let database = Database::new();
    Connection::open(&database.0)
        .unwrap()
        .execute_batch("CREATE TABLE unrelated (value INTEGER)")
        .unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    let note = read_startup_note(&database.0);
    assert!(note.contains("Store format: unrecognized"), "{note}");
    assert!(
        note.contains("The store is not a format this daemon recognizes"),
        "{note}"
    );
    assert!(!note.contains("Run a build supporting"), "{note}");
}

#[test]
fn an_old_store_note_says_it_is_no_longer_read() {
    let database = Database::new();
    Connection::open(&database.0)
        .unwrap()
        .execute_batch(
            "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY);
             INSERT INTO agent_store_format VALUES (12);
             CREATE TABLE unrelated (value INTEGER);",
        )
        .unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    let note = read_startup_note(&database.0);
    assert!(
        note.contains("Store format: v12 (no longer read by this daemon)"),
        "{note}"
    );
    assert!(note.contains("Use a build that reads v12"), "{note}");
}

#[test]
fn a_removed_v14_store_note_says_it_is_no_longer_read() {
    let database = Database::new();
    Connection::open(&database.0)
        .unwrap()
        .execute_batch(
            "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY);
             INSERT INTO agent_store_format VALUES (14);
             CREATE TABLE unrelated (value INTEGER);",
        )
        .unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    let note = read_startup_note(&database.0);
    assert!(
        note.contains("Store format: v14 (no longer read by this daemon)"),
        "{note}"
    );
    assert!(note.contains("Use a build that reads v14"), "{note}");
}

#[test]
fn a_failed_open_replaces_the_previous_startup_note() {
    let database = Database::new();
    Connection::open(&database.0)
        .unwrap()
        .execute_batch(
            "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 34));
             INSERT INTO agent_store_format VALUES (34);",
        )
        .unwrap();
    std::fs::write(
        startup_note(&database.0),
        "dimcode-startup-failure v1 pid=1\nCannot open native store /previous.\n  Store format: v999999\n",
    )
    .unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    let note = read_startup_note(&database.0);
    assert!(note.contains("Store format: v34"), "{note}");
    assert!(!note.contains("/previous"), "{note}");
    assert!(!note.contains("v999999"), "{note}");
}

#[test]
fn a_successful_open_removes_a_stale_startup_note() {
    let database = Database::new();
    drop(SqliteStorage::open(database.0.clone()).unwrap());
    std::fs::write(startup_note(&database.0), "stale").unwrap();
    drop(SqliteStorage::open(database.0.clone()).unwrap());
    assert!(!startup_note(&database.0).exists());
}

#[test]
fn a_successful_upgrade_removes_a_stale_startup_note() {
    let database = Database::new();
    drop(v13_store(&database.0));
    std::fs::write(startup_note(&database.0), "stale").unwrap();
    drop(SqliteStorage::open(database.0.clone()).unwrap());
    assert!(!startup_note(&database.0).exists());
    assert_eq!(
        marker(&Connection::open(&database.0).unwrap()),
        vec![i64::from(STORE_FORMAT)]
    );
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

async fn upgrades_in_place_once(database: &Database, connection: Connection, from: u32) {
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();
    let added: Vec<&str> = V16_TABLES
        .iter()
        .filter(|_| from < 16)
        .chain(V15_TABLES.iter().filter(|_| from < 15))
        .chain(["session_catalog"].iter().filter(|_| from < 23))
        .chain(V28_TABLES.iter().filter(|_| from < 28))
        .chain(V29_TABLES.iter().filter(|_| from < 29))
        .chain(V33_TABLES.iter().filter(|_| from < 33))
        .copied()
        .collect();
    let kept: Vec<&str> = BUSINESS_TABLES
        .iter()
        .filter(|table| !added.contains(table))
        .copied()
        .collect();
    insert_documented_records(&connection, &kept);
    if from >= 31 {
        write_documented_context_switches(&connection);
    }
    let member = from >= 27;
    if member {
        connection
            .execute(
                "INSERT INTO agent_sessions (session_id, config, created_at_ms, leader_session_id) \
                 SELECT 'member', config, created_at_ms, 's1' FROM agent_sessions WHERE session_id='s1'",
                [],
            )
            .unwrap();
        // A real store has a catalog row from creation for every Session, so
        // the v34 data-only step's backfill leaves this one as it is.
        connection
            .execute(
                "INSERT INTO session_catalog (session_id, updated_at_ms, preview, cwd) \
                 SELECT 'member', created_at_ms, NULL, json_extract(config, '$.cwd') \
                 FROM agent_sessions WHERE session_id='member'",
                [],
            )
            .unwrap();
    }
    if from >= 30 {
        connection
            .execute(
                "UPDATE agent_sessions SET origin='internal' WHERE session_id='member'",
                [],
            )
            .unwrap();
    }
    let before: Vec<_> = kept.iter().map(|table| rows(&connection, table)).collect();
    assert!(before.iter().filter(|rows| !rows.is_empty()).count() >= 7);
    // A store at or past v36 already holds the member index rows its writes
    // built, which no later step recomputes; the documented records are
    // inserted under a later format, so they get them here.
    if from >= 36 {
        let sessions = connection
            .prepare("SELECT session_id FROM agent_sessions")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for session in sessions {
            dim_app_server::storage::initialize_session_member_index(&connection, &session)
                .unwrap();
        }
    }
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(from));
    let store = SqliteTimelineStore::new(db.clone());
    assert_eq!(store.branches("s1").await.unwrap().len(), 2);
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(STORE_FORMAT));
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    let text: (i64, i64) = connection
        .query_row(
            "SELECT context_render, context_state FROM timeline_events WHERE kind = 'step/text' LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(text, (1, 1));
    let call: (i64, i64) = connection
        .query_row(
            "SELECT context_render, context_state FROM timeline_events WHERE kind = 'model/call' LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(call, (0, 0));
    let after: Vec<_> = kept.iter().map(|table| rows(&connection, table)).collect();
    assert_eq!(after, before);
    if from >= 27 {
        assert_eq!(
            connection
                .query_row(
                    "SELECT origin FROM agent_sessions WHERE session_id='member'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "internal"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT origin FROM agent_sessions WHERE session_id='s1'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            if from >= 30 { "client" } else { "unknown" }
        );
    }
    for table in added {
        if table != "session_catalog" {
            assert!(rows(&connection, table).is_empty());
        }
    }
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM session_catalog", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        // The v34 data-only step keeps the `member` catalog row that exists
        // from `from >= 27` (a real store has one per Session), so the count
        // follows the same gate as the row's creation.
        if member { 2 } else { 1 }
    );

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
            failure_chain: None,
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

#[tokio::test]
async fn a_v13_store_upgrades_in_place_once() {
    let fresh = Database::new();
    let db = SqliteStorage::open(fresh.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    db.close().await.unwrap();
    let connection = Connection::open(&fresh.0).unwrap();
    assert_eq!(schema(&connection), schema_of(STORE_FORMAT));
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    drop(connection);

    let database = Database::new();
    let connection = v13_store(&database.0);
    upgrades_in_place_once(&database, connection, 13).await;
}

#[tokio::test]
async fn a_v15_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v15_store(&database.0);
    assert_eq!(schema(&connection), schema_of(15));
    upgrades_in_place_once(&database, connection, 15).await;
}

#[tokio::test]
async fn a_v16_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 16);
    assert_eq!(schema(&connection), schema_of(16));
    upgrades_in_place_once(&database, connection, 16).await;
}

#[tokio::test]
async fn a_v17_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 17);
    assert_eq!(schema(&connection), schema_of(17));
    upgrades_in_place_once(&database, connection, 17).await;
}

#[tokio::test]
async fn a_v18_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 18);
    assert_eq!(schema(&connection), schema_of(18));
    upgrades_in_place_once(&database, connection, 18).await;
}

#[tokio::test]
async fn a_v19_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 19);
    assert_eq!(schema(&connection), schema_of(19));
    upgrades_in_place_once(&database, connection, 19).await;
}

#[tokio::test]
async fn a_v20_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 20);
    assert_eq!(schema(&connection), schema_of(20));
    upgrades_in_place_once(&database, connection, 20).await;
}

#[tokio::test]
async fn a_v21_store_upgrades_in_place_once_and_restores_typeless_tool_schemas() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 21);
    assert_eq!(schema(&connection), schema_of(21));
    upgrades_in_place_once(&database, connection, 21).await;

    let tools: Vec<ToolDefinition> = serde_json::from_value(serde_json::json!([{
        "name": "lookup", "description": "Look up an item", "source": "static",
        "inputSchema": {"type": "object", "properties": {
            "rule": {"oneOf": [{"const": "allow"}, {"const": "deny"}]}
        }}
    }]))
    .unwrap();
    let configured = record(
        1,
        EventKind::ContextConfigured(ContextConfiguredData {
            timeline: "main".into(),
            revision: "configuration".into(),
            previous: None,
            prompt: None,
            settings: Box::new(ModelConfiguration {
                provider_id: "mock".into(),
                model_id: "test".into(),
                capability_snapshot: None,
                tools: Some(tools.clone()),
                tool_sources: vec![],
                config: None,
                metadata: None,
            }),
            system: None,
            system_source: None,
        }),
    );
    let mut request = ModelRequest::new("mock", "test", vec![Message::text(Role::User, "Hi")]);
    request.tools = Some(Arc::new(tools.clone()));
    let called = record(
        2,
        EventKind::ModelCall(ModelCallData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            request: Arc::new(request),
            configuration: None,
            prompt: None,
            failure_chain: None,
        }),
    );
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("schema", &SessionConfig::default())
        .await
        .unwrap();
    append(
        &store,
        key("schema", "main"),
        vec![configured.clone(), called.clone()],
    )
    .await
    .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let connection = Connection::open(&database.0).unwrap();
    assert_ne!(schema(&connection), schema_of(21));
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    let body: String = connection
        .query_row(
            "SELECT body FROM timeline_content WHERE session_id = 'schema' AND kind = 'tools'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        serde_json::to_value(&tools).unwrap()
    );
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session("schema".into()).await.unwrap();
    let restored = chain_events(&session, "main", ..=2).await.unwrap();
    assert_eq!(restored[0], configured.event);
    assert_eq!(restored[1], called.event);
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn a_type_array_tool_schema_round_trips_on_the_current_store_without_an_upgrade() {
    let tools: Vec<ToolDefinition> = serde_json::from_value(serde_json::json!([{
        "name": "lookup", "description": "Look up an item", "source": "dynamic",
        "inputSchema": {"type": "object", "properties": {
            "space_id": {"type": ["string", "null"]}
        }, "required": ["space_id"]}
    }]))
    .unwrap();
    let canonical = serde_json::json!({
        "type": "object",
        "properties": {"space_id": {"anyOf": [{"type": "string"}, {"type": "null"}]}},
        "required": ["space_id"]
    });
    assert_eq!(
        serde_json::to_value(&tools[0].input_schema).unwrap(),
        canonical
    );
    let configured = record(
        1,
        EventKind::ContextConfigured(ContextConfiguredData {
            timeline: "main".into(),
            revision: "configuration".into(),
            previous: None,
            prompt: None,
            settings: Box::new(ModelConfiguration {
                provider_id: "mock".into(),
                model_id: "test".into(),
                capability_snapshot: None,
                tools: Some(tools.clone()),
                tool_sources: vec![],
                config: None,
                metadata: None,
            }),
            system: None,
            system_source: None,
        }),
    );
    let mut request = ModelRequest::new("mock", "test", vec![Message::text(Role::User, "Hi")]);
    request.tools = Some(Arc::new(tools.clone()));
    let called = record(
        2,
        EventKind::ModelCall(ModelCallData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            request: Arc::new(request),
            configuration: None,
            prompt: None,
            failure_chain: None,
        }),
    );
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("schema", &SessionConfig::default())
        .await
        .unwrap();
    append(
        &store,
        key("schema", "main"),
        vec![configured.clone(), called.clone()],
    )
    .await
    .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        marker(&connection),
        [i64::from(dim_app_server::storage::STORE_FORMAT)]
    );
    let body: String = connection
        .query_row(
            "SELECT body FROM timeline_content WHERE session_id = 'schema' AND kind = 'tools'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        serde_json::to_value(&tools).unwrap(),
        "the stored bytes carry the canonical form"
    );
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(
        db.upgraded_from(),
        None,
        "the store is already the current format"
    );
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent.restore_session("schema".into()).await.unwrap();
    let restored = chain_events(&session, "main", ..=2).await.unwrap();
    assert_eq!(restored[0], configured.event);
    assert_eq!(restored[1], called.event);
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// ADR 010 and 011: a daemon that finds the old isolation policy names it on
/// stderr, where its default log filter would hide a tracing event, and runs
/// with the user's switches instead.
#[cfg(unix)]
#[tokio::test]
async fn the_daemon_names_an_ignored_host_isolation_file() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let home = std::env::temp_dir().join(format!("dim-legacy-policy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("host-isolation.json"),
        r#"{"version":2,"sandbox":"host"}"#,
    )
    .unwrap();
    let socket = PathBuf::from("/tmp").join(format!("dim-legacy-{}/s", uuid::Uuid::new_v4()));
    let model_config = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dim-agent-aimux/examples/offline.json");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
        .args(["--model-config"])
        .arg(model_config)
        .args(["--no-ws", "--unix"])
        .arg(&socket)
        .arg("--home")
        .arg(&home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut ready = String::new();
    tokio::time::timeout(
        Duration::from_secs(30),
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut ready),
    )
    .await
    .unwrap()
    .unwrap();
    if !ready.contains("unixPath") {
        let mut stderr = String::new();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            child.stderr.take().unwrap().read_to_string(&mut stderr),
        )
        .await;
        panic!("server did not start: {ready}{stderr}");
    }
    child.kill().await.unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .await
        .unwrap();
    let path = home.canonicalize().unwrap().join("host-isolation.json");
    let line = format!(
        "Ignoring {}: command isolation is a user setting in /settings > Process isolation; delete the file.",
        path.display()
    );
    assert!(stderr.lines().any(|report| report == line), "{stderr}");
    std::fs::remove_dir_all(&home).unwrap();
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
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
            Duration::from_secs(30),
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
        "Upgraded native store {} from format v13 to v{STORE_FORMAT}; earlier dimcode versions cannot open it.",
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
    let sqlite = root.plugin(dim_app_server::storage::SqliteStoragePlugin::new(
        database.0.clone(),
    ));
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = dim_agent_runtime::Names::copy(&catalog, &build);
    let session = root.plugin(dim_app_server::storage::SessionStorePlugin::new(&names, 0).unwrap());
    (&sqlite).await.unwrap();
    (&session).await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
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
    let directory = root.get::<dim_app_server::storage::SessionStore>().unwrap();
    let retained_db = root.get::<SqliteStorage>().unwrap();
    assert!(
        directory
            .create_owned_session(
                "missing-owner",
                &SessionConfig::default(),
                "unknown",
                2,
                None,
                SessionOrigin::Client,
                None,
            )
            .await
            .is_err()
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM agent_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    directory
        .create_owned_session(
            "owned",
            &SessionConfig::default(),
            "owner",
            2,
            None,
            SessionOrigin::Client,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        connection
            .query_row("SELECT session_id FROM agent_sessions", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "owned"
    );
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
        directory.creation_record("owned").await,
        Err(AgentError::Storage(_))
    ));
    drop(connection);
    let replacement = SqliteStorage::open(database.0.clone()).unwrap();
    retained_db.close().await.unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    replacement.close().await.unwrap();

    let plugin = dim_app_server::storage::SqliteStoragePlugin::new(database.0.clone());
    assert!(rutis::host::DynPlugin::apply(&plugin, &root).await.is_err());
    let reopened = SqliteStorage::open(database.0.clone()).unwrap();
    reopened.close().await.unwrap();
}

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
    parent.write_all(&[1]).unwrap();
    assert!(spawning.join().unwrap().success());
    closed.unwrap();
    let replacement = replacement.expect("successful close must release native ownership while the child still holds inherited descriptors before exec");
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
    store.release_session("session").await.unwrap();
    transaction.commit().unwrap();
    close.await.unwrap();
    receiver.await.unwrap().unwrap();
    store.release_session("session").await.unwrap();
    store.release_session("session").await.unwrap();
    assert_eq!(store.head(&key("session", "main")).await.unwrap(), 2);
    db.close().await.unwrap();
}

/// A v3 snapshot is not selected, converted or rewritten: its interpreter no
/// longer matches, so the read replays the facts and the row stays as stored.
#[tokio::test]
async fn a_v3_snapshot_is_bypassed_and_left_as_stored() {
    use dim_agent_capabilities::history::project_context_history;

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
                EventKind::InputMessage(InputMessageData::text("history", None)),
            ),
            record(
                2,
                EventKind::InputConsumed(InputConsumedData {
                    input_id: "event-1".into(),
                }),
            ),
            record(
                3,
                EventKind::ContextCompaction(completed("event-2", "summary", 2)),
            ),
        ],
    )
    .await
    .unwrap();
    let state = "{\"resume\":{\"branch_id\":\"main\",\"sequence\":2},\"state\":{\"retain_from\":\"event-2\"}}";
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute(
            "INSERT INTO timeline_snapshots VALUES ('session','main',3,?1,'event-3',?2)",
            params!["dim/context-projection/v3", state],
        )
        .unwrap();
    drop(connection);
    assert!(
        store
            .latest_snapshot(&key("session", "main"), 3, "dim/context-projection/v4")
            .await
            .unwrap()
            .is_none(),
        "the candidate read takes one interpreter"
    );
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    let full = dim_agent_capabilities::history::HistoryRenderer::default()
        .replay_context(&timeline, 3)
        .await
        .unwrap();
    let recovered = dim_agent_capabilities::history::HistoryRenderer::default()
        .context(&timeline, 3)
        .await
        .unwrap();
    assert!(recovered.checkpoint.is_none());
    assert_eq!(
        project_context_history(&recovered).unwrap(),
        project_context_history(&full).unwrap()
    );
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT interpreter, state FROM timeline_snapshots",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap(),
        ("dim/context-projection/v3".into(), state.into()),
        "a bypassed snapshot is not converted or backfilled"
    );
}

/// When no history entry is kept, the checkpoint resumes at the context's last
/// event, which can be inside a merged output row: that event's id is not in
/// the identity index, so the read finds it through the page and begins there.
/// The recovered context is the full replay's.
#[tokio::test]
async fn a_resume_inside_a_merged_output_row_restores_the_full_context() {
    use dim_agent_capabilities::history::{project_context_history, projection_checkpoint};

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
            text(4, "RETAINED "),
            text(5, "ANSWER"),
        ],
    )
    .await
    .unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT sequence, last_sequence, event_id FROM timeline_events \
                 WHERE session_id='session' AND branch_id='main' AND sequence=4",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?
                )),
            )
            .unwrap(),
        (4, 5, None),
        "the two deltas share one merged output row, which holds no id"
    );
    drop(connection);
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    let checkpoint = projection_checkpoint(
        &dim_agent_capabilities::history::HistoryRenderer::default()
            .replay_context(&timeline, 5)
            .await
            .unwrap(),
        "event-3",
    )
    .unwrap();
    assert_eq!(
        (
            checkpoint.resume.sequence,
            checkpoint.resume.event_id.as_str()
        ),
        (5, "event-5"),
        "no entry is kept, so the resume falls back to the last event"
    );
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    append(
        &store,
        key("session", "main"),
        vec![record(
            6,
            EventKind::ContextCompaction(completed("event-3", "summary", 5)),
        )],
    )
    .await
    .unwrap();
    let saved = snapshot(6, &checkpoint);
    store
        .save_snapshot(&key("session", "main"), saved)
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    let full = dim_agent_capabilities::history::HistoryRenderer::default()
        .replay_context(&timeline, 6)
        .await
        .unwrap();
    let recovered = dim_agent_capabilities::history::HistoryRenderer::default()
        .context(&timeline, 6)
        .await
        .unwrap();
    assert!(recovered.checkpoint.is_some());
    assert_eq!(
        recovered.events.first().map(|event| event.id.as_str()),
        Some("event-5"),
        "the read begins at the event the id names inside the merged row"
    );
    assert_eq!(
        project_context_history(&recovered).unwrap(),
        project_context_history(&full).unwrap(),
        "the resumed context is the full replay"
    );
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    store.close().await.unwrap();
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
    let checkpoint = projection_checkpoint(
        &dim_agent_capabilities::history::HistoryRenderer::default()
            .replay_context(&timeline, 5)
            .await
            .unwrap(),
        "event-3",
    )
    .unwrap();
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
        let full = dim_agent_capabilities::history::HistoryRenderer::default()
            .replay_context(&timeline, 6)
            .await
            .unwrap();
        let recovered = dim_agent_capabilities::history::HistoryRenderer::default()
            .context(&timeline, 6)
            .await
            .unwrap();
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
            context_messages(&full, &project_context_history(&full).unwrap())
                .into_iter()
                .map(Arc::unwrap_or_clone)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            context_messages(&recovered, &project_context_history(&recovered).unwrap())
                .into_iter()
                .map(Arc::unwrap_or_clone)
                .collect::<Vec<_>>(),
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
    for (branch, sequence) in [("unknown", 1), ("main", 0), ("main", 3), ("main", 2)] {
        states.push(Some((
            INTERPRETER,
            snapshot(
                3,
                &ProjectionCheckpoint {
                    resume: ResumePoint {
                        branch_id: branch.into(),
                        sequence,
                        event_id: "missing".into(),
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
        let full = dim_agent_capabilities::history::HistoryRenderer::default()
            .replay_context(&timeline, 3)
            .await
            .unwrap();
        assert!(full.checkpoint.is_none());
        let accelerated = dim_agent_capabilities::history::HistoryRenderer::default()
            .context(&timeline, 3)
            .await
            .unwrap();
        assert!(accelerated.checkpoint.is_none());
        use dim_agent_capabilities::history::{
            context_messages, project_context_history, summary_message,
        };
        assert_eq!(
            context_messages(
                &accelerated,
                &project_context_history(&accelerated).unwrap()
            )
            .into_iter()
            .map(Arc::unwrap_or_clone)
            .collect::<Vec<_>>(),
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
        let agent = MinimalHost::start(&builder).await.unwrap();
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
    let checkpoint = projection_checkpoint(
        &dim_agent_capabilities::history::HistoryRenderer::default()
            .replay_context(&prefix, 3)
            .await
            .unwrap(),
        "event-3",
    )
    .unwrap();
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
    let early_context = dim_agent_capabilities::history::HistoryRenderer::default()
        .context(&early, 0)
        .await
        .unwrap();
    assert!(early_context.checkpoint.is_none());
    assert_eq!(early_context.events.len(), 2);
    assert_eq!(
        project_context_history(&early_context).unwrap()[0]
            .message
            .as_ref()
            .clone(),
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
    let context = dim_agent_capabilities::history::HistoryRenderer::default()
        .context(&grandchild, 0)
        .await
        .unwrap();
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
        context_messages(&context, &project_context_history(&context).unwrap())
            .into_iter()
            .map(Arc::unwrap_or_clone)
            .collect::<Vec<_>>(),
        [
            dim_agent_capabilities::history::summary_message("earlier summary"),
            Message::text(Role::User, "queued across compaction")
        ]
    );
    let history = timeline_events(&grandchild, TimelineQueryScope::Chain, ..)
        .await
        .unwrap();
    assert_eq!(history.len(), 6);
    assert_eq!(
        history[..4],
        timeline_events(&main, TimelineQueryScope::Branch, ..=4)
            .await
            .unwrap()
    );
    assert_eq!(
        history[4..],
        timeline_events(&child, TimelineQueryScope::Branch, ..=2)
            .await
            .unwrap()
    );
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
        let agent = MinimalHost::start(&builder).await.unwrap();
        let bad = agent.create_session(SessionConfig::default()).await.unwrap();
        let good = agent.create_session(SessionConfig::default()).await.unwrap();
        bad.submit("stored request".into(), InputMode::Queue, "main").await.unwrap();
        bad.idle("main").await.unwrap();
        let connection = Connection::open(&database.0).unwrap();
        assert!(connection.execute("UPDATE timeline_content SET body = 'unreadable external bytes' WHERE session_id = ?1", [bad.id()]).unwrap() > 0);
        assert!(matches!(chain_events(&bad, "main", ..).await, Err(AgentError::Storage(_))));
        assert!(bad.failure().is_none(), "a read error belongs to its operation");
        good.submit("healthy progress".into(), InputMode::Queue, "main").await.unwrap();
        good.idle("main").await.unwrap();
        assert!(chain_events(&good, "main", ..).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("healthy progress"))));
        assert_eq!(connection.query_row("SELECT body FROM timeline_content WHERE session_id = ?1 LIMIT 1", [bad.id()], |row| row.get::<_, String>(0)).unwrap(), "unreadable external bytes");
        agent.close().await.unwrap();
        store.branches(bad.id()).await.unwrap();
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
    let fitting = SqliteTimelineStore::with_content_budget(db.clone(), 256);
    let small = SqliteTimelineStore::with_content_budget(db.clone(), 150);
    let checking = SqliteTimelineStore::new(db.clone());
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
                    failure_chain: None,
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
            }
            checking.branches(session).await.unwrap();
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
    checking.close().await.unwrap();
    small.close().await.unwrap();
    fitting.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn directory_history_pages_keep_a_fixed_head() {
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
    let first = page(&store, "paged", "main", 0, 200, through).await;
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
    let second = page(&store, "paged", "main", 200, 200, through).await;
    let third = page(&store, "paged", "main", 400, 200, through).await;
    let sequences: Vec<_> = first
        .iter()
        .chain(&second)
        .chain(&third)
        .map(|event| event.sequence)
        .collect();
    assert_eq!(sequences, (1..=600).collect::<Vec<_>>());
    for range in [0..=0, 0..=1, 2..=2, 199..=201, 600..=601] {
        let expected: Vec<_> = range.clone().filter(|sequence| *sequence > 0).collect();
        assert_eq!(
            store
                .read(&key("paged", "main"), range)
                .await
                .unwrap()
                .iter()
                .map(|record| record.event.sequence)
                .collect::<Vec<_>>(),
            expected
        );
    }
    assert!(
        page(&store, "paged", "main", 600, 200, through)
            .await
            .is_empty()
    );
    let connection = Connection::open(&database.0).unwrap();
    let plan = connection
        .prepare("EXPLAIN QUERY PLAN SELECT sequence, last_sequence, kind, body FROM timeline_events WHERE session_id = ?1 AND branch_id = ?2 AND sequence >= coalesce((SELECT max(sequence) FROM timeline_events WHERE session_id = ?1 AND branch_id = ?2 AND sequence <= ?3), ?3) AND sequence <= ?4 AND last_sequence >= ?3 ORDER BY sequence")
        .unwrap()
        .query_map(params!["paged", "main", 201, 400], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(plan.contains("sequence>? AND sequence<?"), "{plan}");
    assert!(!plan.contains("timeline_events_by_end"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
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
        let checkpoint = projection_checkpoint(
            &dim_agent_capabilities::history::HistoryRenderer::default()
                .replay_context(&timeline, 6)
                .await
                .unwrap(),
            "event-3",
        )
        .unwrap();
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
                        failure_chain: None,
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
            let history = project_context_history(
                &dim_agent_capabilities::history::HistoryRenderer::default()
                    .context(&timeline, through)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                history
                    .iter()
                    .map(|entry| entry.message.as_ref())
                    .collect::<Vec<_>>(),
                vec![&assistant]
            );
            let events = timeline_events(&timeline, TimelineQueryScope::Chain, ..)
                .await
                .unwrap();
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
        let anchor = timeline_events(
            &timeline,
            TimelineQueryScope::Branch,
            saved.sequence..=saved.sequence,
        )
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
                failure_chain: None,
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
    append(&store, key("source", "main"), vec![call(1, &["first"])])
        .await
        .unwrap();
    store.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    as_v16_rows(&connection, "source");
    let store = SqliteTimelineStore::new(db.clone());
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

    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();
    sessions
        .fork_session("source", "fork", 4, &config)
        .await
        .unwrap();
    let column = |sql: &str| -> Vec<String> {
        connection
            .prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
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

    // The source can disappear after the caller's read; the copy still
    // refuses a source the transaction cannot find.
    assert!(matches!(
        sessions.fork_session("missing", "orphan", 0, "{}").await,
        Err(AgentError::Closed(_))
    ));
    assert!(column("SELECT session_id FROM agent_sessions WHERE session_id='orphan'").is_empty());
    drop((connection, sessions));
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// #1330: the first thinking event of a block records the block's start, so
/// the span it forms with the next event survives a reload and a fork.
#[tokio::test]
async fn a_thinking_blocks_start_survives_a_reload_and_a_fork() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("source", &SessionConfig::default())
        .await
        .unwrap();
    let mut opened = record(
        1,
        EventKind::Thinking(DeltaData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            delta: "weigh".into(),
        }),
    );
    Arc::make_mut(&mut opened.event).create_at = 1_000;
    let mut answer = text(2, "answer");
    Arc::make_mut(&mut answer.event).create_at = 6_000;
    append(&store, key("source", "main"), vec![opened, answer])
        .await
        .unwrap();
    store.close().await.unwrap();

    let store = SqliteTimelineStore::new(db.clone());
    let read = store.read(&key("source", "main"), 1..=2).await.unwrap();
    assert_eq!(read[0].event.create_at, 1_000);
    assert_eq!(read[1].event.create_at, 6_000);

    let config = store.session_config_text("source").await.unwrap().unwrap();
    store
        .fork_session("source", "fork", 2, &config)
        .await
        .unwrap();
    let forked = store.read(&key("fork", "main"), 1..=2).await.unwrap();
    assert_eq!(forked[0].event.create_at, 1_000);
    assert_eq!(forked[1].event.create_at, 6_000);
}

#[tokio::test]
async fn a_v22_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 22);
    upgrades_in_place_once(&database, connection, 22).await;
}

/// An exact v23 store preserves its rows through subsequent format steps.
#[tokio::test]
async fn a_v23_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = catalog_store(&database.0, 23);
    assert_eq!(schema(&connection), schema_of(23));
    upgrades_in_place_once(&database, connection, 23).await;
}

#[tokio::test]
async fn a_v24_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = catalog_store(&database.0, 24);
    upgrades_in_place_once(&database, connection, 24).await;
}

/// An exact v25 store upgrades by rewriting Provider records only.
#[tokio::test]
async fn a_v25_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = v25_store(&database.0);
    assert_eq!(schema(&connection), schema_of(25));
    upgrades_in_place_once(&database, connection, 25).await;
}

/// #522: the upgrade deletes an unmarked stored limit only from projection
/// output, the remote listing of a record with `remoteModels`, which is
/// re-derived anyway. A record's own models and every user model hold what
/// the client supplied, and the earlier formats wrote no mark there, so
/// each field is decided on its own: a value equal to the old assumed pair
/// is deleted as a possible probe fill, any other value is kept and marked
/// as the declaration it is. A user model that matches a bundled entry keeps
/// its own window over the bundle's.
#[tokio::test]
async fn a_v25_provider_record_keeps_supplied_limits_and_drops_assumed_ones() {
    use serde_json::json;
    let database = Database::new();
    let connection = v25_store(&database.0);
    let caps = |context: u64, output: u64| {
        json!({"modalities":["text"],"toolCalling":true,"streaming":true,
            "structuredOutput":true,"vision":false,"audio":false,"reasoning":false,
            "contextWindow":context,"maxOutputTokens":output})
    };
    let kept = |context: Option<u64>, output: Option<u64>| {
        let mut capabilities = json!({"modalities":["text"],"toolCalling":true,
            "streaming":true,"structuredOutput":true,"vision":false,"audio":false,
            "reasoning":false});
        if let Some(context) = context {
            capabilities["contextWindow"] = json!(context);
        }
        if let Some(output) = output {
            capabilities["maxOutputTokens"] = json!(output);
        }
        capabilities
    };
    // `provider.refresh` wrote these from the remote listing.
    let listed_record = json!({
        "kind":"custom","displayName":"Listed","driverKind":"openai-compatible",
        "auth":{"type":"anonymous"},"credentialAuthority":"static",
        "baseUrl":"http://127.0.0.1:1/v1","network":{},"headers":{},"credential":null,
        "models":[
            {"modelId":"listed-both","displayName":"Listed Both","status":"active",
             "capabilities":caps(1_000_000,65_536),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"],
                "raw":{"id":"listed-both","inputTokenLimit":1_000_000,"outputTokenLimit":65_536}}},
            {"modelId":"listed-window","displayName":"Listed Window","status":"active",
             "capabilities":caps(32_000,4_000),
             "metadata":{"raw":{"id":"listed-window","inputTokenLimit":32_000}}},
            {"modelId":"listed-none","displayName":"Listed None","status":"active",
             "capabilities":caps(8_000,1_000),"metadata":{"raw":{"id":"listed-none","type":"chat"}}}
        ],
        "remoteModels":true,
        "modelConfigOverrides":[
            {"modelId":"renamed-listing","displayName":"Renamed","status":"active",
             "capabilities":caps(128_000,16_384),"metadata":{"raw":{}}},
            {"modelId":"override-marked","displayName":"Override","status":"active",
             "capabilities":caps(64_000,8_000),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"],"raw":{}}},
            // The importer copied this override from the legacy data, which
            // stored the limits a user typed and wrote no mark.
            {"modelId":"imported-override","displayName":"Imported","status":"active",
             "capabilities":caps(200_000,32_000),"metadata":{"raw":{}}}
        ],
        "userModels":[
            {"modelId":"user-model","displayName":"User","status":"active",
             "capabilities":caps(32_000,4_000)},
            {"modelId":"user-assumed","displayName":"Assumed","status":"active",
             "capabilities":caps(128_000,16_384)},
            {"modelId":"user-mixed","displayName":"Mixed","status":"active",
             "capabilities":caps(32_000,16_384)},
            {"modelId":"user-typed","displayName":"Typed","status":"active",
             "capabilities":caps(64_000,8_000)}
        ],
        "disabledModelIds":[],"enabled":true,"definitionFingerprint":null,"version":1
    });
    // `provider.create` wrote these from its probe; the user model came from
    // `provider.models.create`. Neither path marked the limits.
    let custom_record = json!({
        "kind":"custom","displayName":"Hand Entered","driverKind":"openai-compatible",
        "auth":{"type":"anonymous"},"credentialAuthority":"static",
        "baseUrl":"http://127.0.0.1:1/v1","network":{},"headers":{},"credential":null,
        "models":[
            {"modelId":"hand-entered","displayName":"Hand Entered","status":"active",
             "capabilities":caps(32_000,4_000),
             "metadata":{"raw":{"id":"hand-entered","providerId":"custom-provider"}}},
            {"modelId":"declared-listing","displayName":"Declared","status":"active",
             "capabilities":caps(64_000,8_000),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"],
                "raw":{"id":"declared-listing","inputTokenLimit":64_000,"outputTokenLimit":8_000}}}
        ],
        "remoteModels":false,
        "modelConfigOverrides":[],
        "disabledModelIds":[],"enabled":true,"definitionFingerprint":null,"version":1
    });
    // A builtin whose listing does not hold the user's model ID: the bundled
    // catalog declares 400000/128000 for it, the user's model 32000/4000.
    let builtin_record = json!({
        "kind":"builtin","displayName":"OpenAI","driverKind":"openai-responses",
        "auth":{"type":"api_key","headerName":"Authorization","prefix":"Bearer "},
        "credentialAuthority":"static",
        "baseUrl":"https://api.openai.com/v1","network":{},"headers":{},"credential":null,
        "models":[
            {"modelId":"gpt-5.4-mini","displayName":"GPT-5.4 Mini","status":"active",
             "capabilities":caps(400_000,128_000),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"],
                "raw":{"id":"gpt-5.4-mini","inputTokenLimit":400_000,"outputTokenLimit":128_000}}}
        ],
        "remoteModels":true,
        "modelConfigOverrides":[],
        "userModels":[
            {"modelId":"gpt-5-nano","displayName":"GPT-5 Nano","status":"active",
             "capabilities":caps(32_000,4_000)}
        ],
        "disabledModelIds":[],"enabled":true,"definitionFingerprint":null,"version":1
    });
    let managed_record = json!({
        "kind":"managed",
        "account":{"credentialEpoch":"epoch","generation":1,
            "account":{"issuer":"issuer","subject":"subject","grantedScopes":[]}},
        "models":[{"entry":{"modelId":"relay-model","displayName":"Relay","status":"active",
            "capabilities":caps(128_000,16_384),"metadata":{"raw":{}}},
            "protocol":"openai-compatible","zdr":false}],
        "modelConfigOverrides":[],"disabledModelIds":[],"modelPreferences":{},"version":1
    });
    // A builtin record that never refreshed: its models are bundle-derived
    // with `remoteModels` false. The bundle states the limits in `raw.limit`,
    // so the strict rule keeps what the bundle declares and drops an unmarked
    // limit no raw item declares.
    let seeded_record = json!({
        "kind":"builtin","displayName":"Anthropic","driverKind":"anthropic",
        "auth":{"type":"api_key","headerName":"x-api-key","prefix":""},
        "credentialAuthority":"static",
        "baseUrl":"https://api.anthropic.com/v1","network":{},"headers":{},"credential":null,
        "models":[
            {"modelId":"claude-bundled","displayName":"Claude Bundled","status":"active",
             "capabilities":caps(200_000,64_000),
             "metadata":{"raw":{"id":"claude-bundled","limit":{"context":200_000,"output":64_000}}}},
            {"modelId":"claude-unlisted","displayName":"Claude Unlisted","status":"active",
             "capabilities":caps(200_000,8_000),"metadata":{"raw":{"id":"claude-unlisted"}}}
        ],
        "remoteModels":false,
        "modelConfigOverrides":[],
        "userModels":[
            {"modelId":"claude-user","displayName":"Claude User","status":"active",
             "capabilities":caps(32_000,16_384)}
        ],
        "disabledModelIds":[],"enabled":true,"definitionFingerprint":null,"version":1
    });
    for (id, record) in [
        ("listed-provider", &listed_record),
        ("custom-provider", &custom_record),
        ("openai", &builtin_record),
        ("managed-provider", &managed_record),
        ("anthropic", &seeded_record),
    ] {
        connection
            .execute(
                "INSERT INTO providers(provider_id,version,record) VALUES (?1,1,?2)",
                params![id, record.to_string()],
            )
            .unwrap();
    }
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(25));
    db.close().await.unwrap();

    let read = |id: &str| -> String {
        Connection::open(&database.0)
            .unwrap()
            .query_row(
                "SELECT record FROM providers WHERE provider_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(STORE_FORMAT));
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    drop(connection);

    let listed_expected = json!({
        "kind":"custom","displayName":"Listed","driverKind":"openai-compatible",
        "auth":{"type":"anonymous"},"credentialAuthority":"static",
        "baseUrl":"http://127.0.0.1:1/v1","network":{},"headers":{},"credential":null,
        "models":[
            {"modelId":"listed-both","displayName":"Listed Both","status":"active",
             "capabilities":kept(Some(1_000_000),Some(65_536)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"],
                "raw":{"id":"listed-both","inputTokenLimit":1_000_000,"outputTokenLimit":65_536}}},
            {"modelId":"listed-window","displayName":"Listed Window","status":"active",
             "capabilities":kept(Some(32_000),None),
             "metadata":{"raw":{"id":"listed-window","inputTokenLimit":32_000}}},
            {"modelId":"listed-none","displayName":"Listed None","status":"active",
             "capabilities":kept(None,None),"metadata":{"raw":{"id":"listed-none","type":"chat"}}}
        ],
        "remoteModels":true,
        "modelConfigOverrides":[
            {"modelId":"renamed-listing","displayName":"Renamed","status":"active",
             "capabilities":kept(None,None),"metadata":{}},
            {"modelId":"override-marked","displayName":"Override","status":"active",
             "capabilities":kept(Some(64_000),Some(8_000)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"]}},
            {"modelId":"imported-override","displayName":"Imported","status":"active",
             "capabilities":kept(Some(200_000),Some(32_000)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"]}}
        ],
        "userModels":[
            {"modelId":"user-model","displayName":"User","status":"active",
             "capabilities":kept(Some(32_000),Some(4_000)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"]}},
            {"modelId":"user-assumed","displayName":"Assumed","status":"active",
             "capabilities":kept(None,None)},
            {"modelId":"user-mixed","displayName":"Mixed","status":"active",
             "capabilities":kept(Some(32_000),None),
             "metadata":{"capabilityOverrideFields":["contextWindow"]}},
            {"modelId":"user-typed","displayName":"Typed","status":"active",
             "capabilities":kept(Some(64_000),Some(8_000)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"]}}
        ],
        "disabledModelIds":[],"enabled":true,"definitionFingerprint":null,"version":1
    });
    let custom_expected = json!({
        "kind":"custom","displayName":"Hand Entered","driverKind":"openai-compatible",
        "auth":{"type":"anonymous"},"credentialAuthority":"static",
        "baseUrl":"http://127.0.0.1:1/v1","network":{},"headers":{},"credential":null,
        "models":[
            {"modelId":"hand-entered","displayName":"Hand Entered","status":"active",
             "capabilities":kept(Some(32_000),Some(4_000)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"],
                "raw":{"id":"hand-entered","providerId":"custom-provider"}}},
            {"modelId":"declared-listing","displayName":"Declared","status":"active",
             "capabilities":kept(Some(64_000),Some(8_000)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"],
                "raw":{"id":"declared-listing","inputTokenLimit":64_000,"outputTokenLimit":8_000}}}
        ],
        "remoteModels":false,
        "modelConfigOverrides":[],
        "disabledModelIds":[],"enabled":true,"definitionFingerprint":null,"version":1
    });
    let builtin_expected = json!({
        "kind":"builtin","displayName":"OpenAI","driverKind":"openai-responses",
        "auth":{"type":"api_key","headerName":"Authorization","prefix":"Bearer "},
        "credentialAuthority":"static",
        "baseUrl":"https://api.openai.com/v1","network":{},"headers":{},"credential":null,
        "models":[
            {"modelId":"gpt-5.4-mini","displayName":"GPT-5.4 Mini","status":"active",
             "capabilities":kept(Some(400_000),Some(128_000)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"],
                "raw":{"id":"gpt-5.4-mini","inputTokenLimit":400_000,"outputTokenLimit":128_000}}}
        ],
        "remoteModels":true,
        "modelConfigOverrides":[],
        "userModels":[
            {"modelId":"gpt-5-nano","displayName":"GPT-5 Nano","status":"active",
             "capabilities":kept(Some(32_000),Some(4_000)),
             "metadata":{"capabilityOverrideFields":["contextWindow","maxOutputTokens"]}}
        ],
        "disabledModelIds":[],"enabled":true,"definitionFingerprint":null,"version":1
    });
    let seeded_expected = json!({
        "kind":"builtin","displayName":"Anthropic","driverKind":"anthropic",
        "auth":{"type":"api_key","headerName":"x-api-key","prefix":""},
        "credentialAuthority":"static",
        "baseUrl":"https://api.anthropic.com/v1","network":{},"headers":{},"credential":null,
        "models":[
            {"modelId":"claude-bundled","displayName":"Claude Bundled","status":"active",
             "capabilities":kept(Some(200_000),Some(64_000)),
             "metadata":{"raw":{"id":"claude-bundled","limit":{"context":200_000,"output":64_000}}}},
            {"modelId":"claude-unlisted","displayName":"Claude Unlisted","status":"active",
             "capabilities":kept(None,None),"metadata":{"raw":{"id":"claude-unlisted"}}}
        ],
        "remoteModels":false,
        "modelConfigOverrides":[],
        "userModels":[
            {"modelId":"claude-user","displayName":"Claude User","status":"active",
             "capabilities":kept(Some(32_000),None),
             "metadata":{"capabilityOverrideFields":["contextWindow"]}}
        ],
        "disabledModelIds":[],"enabled":true,"definitionFingerprint":null,"version":1
    });
    let managed_expected = json!({
        "kind":"managed",
        "account":{"credentialEpoch":"epoch","generation":1,
            "account":{"issuer":"issuer","subject":"subject","grantedScopes":[]}},
        "models":[{"entry":{"modelId":"relay-model","displayName":"Relay","status":"active",
            "capabilities":kept(None,None),"metadata":{}},
            "protocol":"openai-compatible","zdr":false,"unavailableReason":"limits_undeclared"}],
        "modelConfigOverrides":[],"disabledModelIds":[],"version":1
    });
    for (id, expected) in [
        ("listed-provider", &listed_expected),
        ("custom-provider", &custom_expected),
        ("openai", &builtin_expected),
        ("managed-provider", &managed_expected),
        ("anthropic", &seeded_expected),
    ] {
        let stored: serde_json::Value = serde_json::from_str(&read(id)).unwrap();
        assert_eq!(&stored, expected, "{id}");
    }

    // The user's window is the one in force: the bundled catalog declares
    // 400000/128000 for this ID, so only the mark the upgrade wrote keeps the
    // user's 32000/4000 from being replaced on the next read.
    let stored: serde_json::Value = serde_json::from_str(&read("openai")).unwrap();
    let entries: Vec<dim_app_server::model_catalog::ModelEntry> = stored["models"]
        .as_array()
        .unwrap()
        .iter()
        .chain(stored["userModels"].as_array().unwrap())
        .map(|entry| serde_json::from_value(entry.clone()).unwrap())
        .collect();
    let bundled = dim_app_server::model_catalog::BundledCatalog::load().unwrap();
    let effective = bundled.effective_models(
        &dim_app_server::model_catalog::ProviderProjection {
            provider_id: "openai".into(),
            source: Some(dim_app_server::model_catalog::ModelSource::Builtin),
            models: entries.clone(),
            ..Default::default()
        },
        &entries,
        &[],
    );
    let user = effective
        .iter()
        .find(|entry| entry.model_id == "gpt-5-nano")
        .unwrap();
    assert_eq!(user.capabilities.context_window, Some(32_000));
    assert_eq!(user.capabilities.max_output_tokens, Some(4_000));

    // A second open neither upgrades nor rewrites the records again.
    let before = (
        read("listed-provider"),
        read("custom-provider"),
        read("openai"),
        read("managed-provider"),
        read("anthropic"),
    );
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    db.close().await.unwrap();
    assert_eq!(
        (
            read("listed-provider"),
            read("custom-provider"),
            read("openai"),
            read("managed-provider"),
            read("anthropic")
        ),
        before
    );
}

/// A streamed context folds pages of events, not atomic batches: the page that
/// ends inside the batch at 255..=257 carries part of it, and the projection of
/// the pages still equals the whole render.
#[tokio::test]
async fn a_streamed_context_folds_a_page_that_ends_inside_an_atomic_batch() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let singles: Vec<_> = (1..=254).map(|sequence| text(sequence, "line")).collect();
    append(&store, key("session", "main"), singles)
        .await
        .unwrap();
    let mut batch = vec![text(255, "a"), text(256, "b"), text(257, "c")];
    for record in &mut batch {
        record.batch_end = 257;
    }
    append(&store, key("session", "main"), batch).await.unwrap();
    let timeline = Timeline::restore(
        rutis::Ctx::root().unwrap(),
        key("session", "main"),
        vec![],
        store.clone(),
    )
    .await
    .unwrap();
    let renderer = HistoryRenderer::default();
    let streamed = renderer.rendered_stream(&timeline, 257, 256).await.unwrap();
    let (_, full) = renderer.rendered_context(&timeline, 257).await.unwrap();
    assert_eq!(streamed.history, full);
    db.close().await.unwrap();
}

/// A history page's limit is a target, not an edge: a page that would end inside
/// an atomic batch reads the batch whole.
#[tokio::test]
async fn a_history_page_that_would_end_inside_a_batch_reads_it_whole() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let singles: Vec<_> = (1..=254).map(|sequence| text(sequence, "line")).collect();
    append(&store, key("session", "main"), singles)
        .await
        .unwrap();
    let mut batch = vec![text(255, "a"), text(256, "b"), text(257, "c")];
    for record in &mut batch {
        record.batch_end = 257;
    }
    append(&store, key("session", "main"), batch).await.unwrap();
    for limit in 1..=3 {
        let page = store
            .read_page(&key("session", "main"), 254, limit, 257)
            .await
            .unwrap();
        assert_eq!(
            page.iter()
                .map(|record| record.event.sequence)
                .collect::<Vec<_>>(),
            vec![255, 256, 257],
            "a page whose limit would end inside the atomic batch reads it whole: {limit}"
        );
    }
    db.close().await.unwrap();
}

/// The step that adds a column to two tables alters them before v27's indexes
/// exist, and keeps every business row.
#[tokio::test]
async fn a_v26_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = catalog_store(&database.0, 26);
    connection
        .execute_batch("DROP INDEX timeline_events_by_end")
        .unwrap();
    assert_eq!(schema(&connection), schema_of(26));
    upgrades_in_place_once(&database, connection, 26).await;
}

#[tokio::test]
async fn a_v27_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(27);
    statements.sort_by_key(|sql| !sql.starts_with("CREATE TABLE"));
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (27)", [])
        .unwrap();
    assert_eq!(schema(&connection), schema_of(27));
    upgrades_in_place_once(&database, connection, 27).await;
    let connection = Connection::open(&database.0).unwrap();
    assert!(rows(&connection, "session_grants").is_empty());
}

#[tokio::test]
async fn a_v28_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(28);
    statements.sort_by_key(|sql| !sql.starts_with("CREATE TABLE"));
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (28)", [])
        .unwrap();
    assert_eq!(schema(&connection), schema_of(28));
    upgrades_in_place_once(&database, connection, 28).await;
    let connection = Connection::open(&database.0).unwrap();
    assert!(rows(&connection, "session_output_owners").is_empty());
}

/// #557: the step that adds `agent_sessions.origin` alters the table, keeps
/// every business row, marks the rows whose leader link it already has and
/// leaves the rest unknown.
#[tokio::test]
async fn a_v29_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(29);
    statements.sort_by_key(|sql| !sql.starts_with("CREATE TABLE"));
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (29)", [])
        .unwrap();
    assert_eq!(schema(&connection), schema_of(29));
    upgrades_in_place_once(&database, connection, 29).await;
}

/// #605: the two reading-switch indexes become one, and the switch values and
/// every row stay.
#[tokio::test]
async fn a_v31_store_upgrades_in_place_once() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(31);
    statements.sort_by_key(|sql| !sql.starts_with("CREATE TABLE"));
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (31)", [])
        .unwrap();
    assert_eq!(schema(&connection), schema_of(31));
    upgrades_in_place_once(&database, connection, 31).await;
}

/// #1421: the step that rebuilds `agent_sessions` for the `automation` origin
/// keeps every business row and every Session `internal`, recreates each
/// object `SCHEMA` places on the table, and leaves the SQL of the tables that
/// reference it unchanged.
#[tokio::test]
async fn a_v41_store_upgrades_in_place_once_keeping_internal_launches() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(41);
    statements.sort_by_key(|sql| {
        !sql.starts_with("CREATE TABLE ") && !sql.starts_with("CREATE VIRTUAL TABLE ")
    });
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (41)", [])
        .unwrap();
    assert_eq!(schema(&connection), schema_of(41));
    let referencing: Vec<(String, String)> = connection
        .prepare(
            "SELECT name, sql FROM sqlite_master WHERE type='table' AND name != 'agent_sessions' \
             AND sql LIKE '%REFERENCES agent_sessions(%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        referencing.len() >= 8,
        "every table that references the rebuilt one is compared: {referencing:?}"
    );
    assert!(
        referencing
            .iter()
            .any(|(name, _)| name == "session_catalog"),
        "{referencing:?}"
    );
    upgrades_in_place_once(&database, connection, 41).await;
    let connection = Connection::open(&database.0).unwrap();
    for (name, sql) in &referencing {
        assert_eq!(
            connection
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE name=?1",
                    [name],
                    |row| { row.get::<_, String>(0) }
                )
                .unwrap(),
            *sql,
            "the rebuild leaves the referencing table {name} as it was"
        );
    }
    let indexed: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_master WHERE tbl_name='agent_sessions' AND type='index' AND sql IS NOT NULL ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(indexed, ["agent_sessions_by_leader"]);
    for (session, expected) in [("s1", "client"), ("member", "internal")] {
        let origin: String = connection
            .query_row(
                "SELECT origin FROM agent_sessions WHERE session_id=?1",
                [session],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            origin, expected,
            "{session} keeps the origin it was stored with"
        );
    }
    connection
        .execute(
            "INSERT INTO agent_sessions (session_id, config, created_at_ms, origin) \
             SELECT 'automation', config, created_at_ms, 'automation' FROM agent_sessions WHERE session_id='s1'",
            [],
        )
        .unwrap();
    // The catalog's own condition, over the upgraded table: the row a launch
    // created before this format stays out of the automation class.
    let automation: Vec<String> = connection
        .prepare("SELECT session_id FROM agent_sessions WHERE origin != 'automation' ORDER BY session_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(automation, ["member", "s1"]);
    assert!(
        connection
            .execute(
                "INSERT INTO agent_sessions (session_id, config, created_at_ms, origin) \
                 SELECT 'bogus', config, created_at_ms, 'bogus' FROM agent_sessions WHERE session_id='s1'",
                [],
            )
            .is_err(),
        "the widened CHECK still refuses an origin it does not name"
    );
}

/// A v41 store whose upgrade runs with foreign keys off, as the startup
/// transaction does, and whose rows are the documented example: the shape the
/// interrupted-state tests start from.
fn v41_store_with_example_rows(database: &Database) -> Connection {
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(41);
    statements.sort_by_key(|sql| {
        !sql.starts_with("CREATE TABLE ") && !sql.starts_with("CREATE VIRTUAL TABLE ")
    });
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (41)", [])
        .unwrap();
    insert_documented_records(&connection, BUSINESS_TABLES);
    write_documented_context_switches(&connection);
    for session in connection
        .prepare("SELECT session_id FROM agent_sessions")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    {
        dim_app_server::storage::initialize_session_member_index(&connection, &session).unwrap();
    }
    assert_eq!(schema(&connection), schema_of(41));
    connection
}

/// #1421: an upgrade passes over the rows an interrupted `drop_session` left.
/// It removes `timeline_branches` and `agent_sessions` in its first chunk and
/// the events in later ones, so an interruption leaves `timeline_events`
/// naming a branch and a Session that are gone. The store documents that state
/// and a retry converges, so the upgrade must neither refuse it nor repair it.
#[tokio::test]
async fn a_v41_store_interrupted_mid_delete_upgrades_and_keeps_its_orphans() {
    let database = Database::new();
    let connection = v41_store_with_example_rows(&database);
    // The first chunk committed; the event chunks did not.
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             DELETE FROM session_catalog WHERE session_id='s1';
             DELETE FROM session_owners WHERE session_id='s1';
             DELETE FROM timeline_branches WHERE session_id='s1';
             DELETE FROM agent_sessions WHERE session_id='s1';",
        )
        .unwrap();
    let events: i64 = connection
        .query_row(
            "SELECT count(*) FROM timeline_events WHERE session_id='s1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(events > 0, "the interrupted delete left its events behind");
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(41));
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_events WHERE session_id='s1'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        events,
        "the orphaned events are still there"
    );
    let dangling: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(dangling >= events, "the orphans still dangle");
}

/// #1421: the same for an interrupted `copy_fork_rows`, which writes the
/// fork's branch rows before its Session row: an interruption leaves a branch
/// naming a Session that does not exist yet.
#[tokio::test]
async fn a_v41_store_interrupted_mid_fork_upgrades_and_keeps_its_orphans() {
    let database = Database::new();
    let connection = v41_store_with_example_rows(&database);
    // `copy_fork_rows` writes the branch row in its first chunk and the
    // Session row in its last, so an interruption names exactly
    // `timeline_branches → agent_sessions`.
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO timeline_branches VALUES ('fork', 'main', '{\"id\":\"main\"}', 0);",
        )
        .unwrap();
    let dangling: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(dangling, 1, "the fork's branch names no Session");
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(41));
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_branches WHERE session_id='fork'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1,
        "the interrupted fork's branch is still there"
    );
    let dangling: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(dangling, 1, "and it still dangles");
}

/// A store upgrade that outlives `startup_timeout` still opens while it
/// reports progress, and the CLI's progress note exists while it runs and is
/// gone after the successful open.
#[tokio::test]
async fn a_slow_store_upgrade_reports_progress_and_keeps_the_opening_alive() {
    use dim_app_server::storage::set_upgrade_report_delay;
    use tokio_util::sync::CancellationToken;

    /// Clears the delay even when an assertion fails.
    struct Delay;
    impl Drop for Delay {
        fn drop(&mut self) {
            set_upgrade_report_delay(None);
        }
    }

    let database = Database::new();
    drop(v13_store(&database.0));

    let _delay = Delay;
    set_upgrade_report_delay(Some(Duration::from_millis(200)));
    let note = startup_note(&database.0);
    let path = database.0.clone();
    let waiting = tokio::spawn(async move {
        let root = rutis::Ctx::root().unwrap();
        let view = root.plugin(dim_app_server::storage::SqliteStoragePlugin::new(path));
        let result = dim_agent_runtime::wait_active(
            &root,
            std::slice::from_ref(&view),
            Duration::from_millis(1_000),
            &CancellationToken::new(),
        )
        .await;
        root.shutdown().await.unwrap();
        result
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = None;
    while Instant::now() < deadline && seen.is_none() {
        seen = std::fs::read_to_string(&note).ok().filter(|text| {
            text.starts_with(&format!(
                "dimcode-startup-progress v1 pid={}\n",
                std::process::id()
            ))
        });
        if seen.is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    let seen = seen.expect("the upgrade wrote its progress note while it ran");
    assert!(
        seen.contains(&format!(
            "store-upgrade from=13 to={STORE_FORMAT} progress="
        )),
        "{seen}"
    );
    waiting
        .await
        .unwrap()
        .expect("reported progress keeps the opening inside its inactivity bound");
    assert!(
        !note.exists(),
        "a successful open removes the progress note"
    );
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(marker(&connection), vec![i64::from(STORE_FORMAT)]);
}

/// Grants upsert without downgrading write, survive a restart, are absent
/// from a fork, and remove takes the same path it added.
#[tokio::test]
async fn session_grants_upsert_remove_survive_restart_and_are_not_forked() {
    let database = Database::new();
    let notes = PathBuf::from("/work/notes.md");
    let directory = PathBuf::from("/work/notes");

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions =
        dim_app_server::storage::SessionStore::new(db.clone(), rutis::Ctx::root().unwrap(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("s", &SessionConfig::default())
        .await
        .unwrap();
    sessions
        .add_grant("s", notes.clone(), GrantKind::File, GrantMode::Write)
        .await
        .unwrap();
    sessions
        .add_grant("s", notes.clone(), GrantKind::File, GrantMode::Read)
        .await
        .unwrap();
    sessions
        .add_grant("s", directory.clone(), GrantKind::Dir, GrantMode::Write)
        .await
        .unwrap();
    assert_eq!(
        sessions.grants("s").await.unwrap(),
        vec![
            SessionGrant {
                path: directory,
                kind: GrantKind::Dir,
                mode: GrantMode::Write,
            },
            SessionGrant {
                path: notes.clone(),
                kind: GrantKind::File,
                mode: GrantMode::Write,
            },
        ]
    );

    let config = sessions.session_config_text("s").await.unwrap().unwrap();
    sessions.fork_session("s", "f", 0, &config).await.unwrap();
    assert!(sessions.grants("f").await.unwrap().is_empty());

    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions =
        dim_app_server::storage::SessionStore::new(db.clone(), rutis::Ctx::root().unwrap(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    assert_eq!(sessions.grants("s").await.unwrap().len(), 2);
    sessions.remove_grant("s", notes.clone()).await.unwrap();
    assert_eq!(
        sessions.grants("s").await.unwrap(),
        vec![SessionGrant {
            path: PathBuf::from("/work/notes"),
            kind: GrantKind::Dir,
            mode: GrantMode::Write,
        }]
    );

    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// ADR 013 D4: output rows are registered, copied by a fork and deleted with
/// their Session; a directory another Session still names is kept. `owns`
/// answers from the output table and the Session file table.
#[tokio::test]
async fn output_ownership_survives_a_fork_and_a_source_delete() {
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

    let output = directory.join("command-output/source/exec.log");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(&output, b"output").unwrap();
    sessions.register("source", output.clone()).await.unwrap();
    assert!(sessions.owns("source", output.clone()).await.unwrap());
    assert!(
        !sessions
            .owns("source", directory.join("other.log"))
            .await
            .unwrap()
    );
    assert!(
        !sessions
            .owns(
                "source",
                directory.join("command-output/source/../source/exec.log")
            )
            .await
            .unwrap()
    );
    #[cfg(unix)]
    {
        let link = directory.join("command-output/source/link.log");
        std::os::unix::fs::symlink(&output, &link).unwrap();
        assert!(!sessions.owns("source", link).await.unwrap());
    }

    let file_id = "file-1";
    let attachment = directory
        .join("dimcode.sqlite.session-files")
        .join(file_id)
        .join("paste.txt");
    std::fs::create_dir_all(attachment.parent().unwrap()).unwrap();
    std::fs::write(&attachment, b"pasted").unwrap();
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute("INSERT INTO session_files (file_id) VALUES (?1)", [file_id])
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_file_owners (session_id, file_id) VALUES ('source', ?1)",
            [file_id],
        )
        .unwrap();
    drop(connection);
    assert!(sessions.owns("source", attachment.clone()).await.unwrap());
    assert!(
        !sessions
            .owns(
                "source",
                directory.join("dimcode.sqlite.session-files/another/paste.txt")
            )
            .await
            .unwrap()
    );

    let config = sessions
        .session_config_text("source")
        .await
        .unwrap()
        .unwrap();
    sessions
        .fork_session("source", "fork", 0, &config)
        .await
        .unwrap();
    assert!(sessions.owns("fork", output.clone()).await.unwrap());
    assert!(sessions.owns("fork", attachment.clone()).await.unwrap());
    assert_eq!(
        sessions.owned_paths("fork").await.unwrap(),
        vec![
            directory.join("dimcode.sqlite.session-files").join(file_id),
            output.clone(),
        ]
    );

    assert!(sessions.delete_session("source").await.unwrap());
    assert!(output.exists());
    assert!(attachment.exists());
    assert!(!sessions.owns("source", output.clone()).await.unwrap());
    assert!(sessions.owns("fork", output.clone()).await.unwrap());

    assert!(sessions.delete_session("fork").await.unwrap());
    assert!(!output.exists());
    assert!(!sessions.owns("fork", output).await.unwrap());

    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_derived_indexes_are_created_at_open_without_a_format_step() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    let indexed = |connection: &Connection, name: &str| -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                [name],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert!(indexed(&connection, "timeline_history_anchors"));
    assert!(indexed(&connection, "timeline_policy_turns"));
    assert!(indexed(&connection, "timeline_events_by_kind"));
    let columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_index_info('timeline_policy_turns')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        columns,
        ["session_id", "branch_id", "kind", "sequence", "event_id"],
        "the retained policy index keeps the columns a reader would need"
    );
    let plan: Vec<String> = connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT event_id, sequence FROM timeline_events \
             WHERE session_id=?1 AND branch_id=?2 AND kind='turn/start' \
             AND sequence BETWEEN ?3 AND ?4 AND event_id IS NOT NULL ORDER BY sequence",
        )
        .unwrap()
        .query_map(params!["session", "main", 1, 2], |row| row.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.contains("COVERING INDEX timeline_policy_turns")),
        "the policy index covers the columns it holds: {plan:?}"
    );
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    connection
        .execute_batch("DROP INDEX timeline_history_anchors; DROP INDEX timeline_policy_turns")
        .unwrap();
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(
        db.upgraded_from(),
        None,
        "the indexes are not a format step"
    );
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    assert!(
        indexed(&connection, "timeline_history_anchors"),
        "a later open recreates the index"
    );
    assert!(
        indexed(&connection, "timeline_policy_turns"),
        "a later open recreates the index"
    );
    assert!(
        indexed(&connection, "timeline_events_by_kind"),
        "a later open recreates the index"
    );
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    drop(connection);

    let (read, format) = SqliteStorage::open_read_only(database.0.clone()).unwrap();
    assert_eq!(format, STORE_FORMAT);
    read.close().await.unwrap();
}

/// A batch's edge is answered from the row that covers the sequence: an
/// interior sequence is refused and a batch's last sequence passes, also when
/// one physical row holds the records of several batches.
#[tokio::test]
async fn a_batch_edge_check_reads_the_row_covering_the_sequence() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    append(&store, key("session", "main"), vec![text(1, "one")])
        .await
        .unwrap();
    let mut two = text(2, "two");
    two.batch_end = 3;
    append(&store, key("session", "main"), vec![two, text(3, "three")])
        .await
        .unwrap();
    let mut four = record(
        4,
        EventKind::InputMessage(InputMessageData::text("first", None)),
    );
    four.batch_end = 5;
    append(
        &store,
        key("session", "main"),
        vec![
            four,
            record(
                5,
                EventKind::InputMessage(InputMessageData::text("second", None)),
            ),
        ],
    )
    .await
    .unwrap();

    for edge in [1, 3, 5] {
        store
            .batch_end(&key("session", "main"), edge)
            .await
            .unwrap_or_else(|error| panic!("sequence {edge} ends a batch: {error:?}"));
    }
    for inside in [2, 4] {
        assert!(matches!(
            store.batch_end(&key("session", "main"), inside).await,
            Err(AgentError::InvalidInput(message))
                if message == format!("sequence {inside} is inside an atomic batch")
        ));
    }
    assert!(matches!(
        store.batch_end(&key("session", "main"), 6).await,
        Err(AgentError::Storage(_))
    ));

    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// One stored fact for the query flows, with its atomic batch's last
/// sequence.
fn query_record(sequence: u64, batch_end: u64, id: &str, kind: EventKind) -> StoredEvent {
    StoredEvent {
        event: Arc::new(AgentEvent {
            id: id.into(),
            sequence,
            create_at: 1000 + sequence,
            tags: None,
            parent_call_id: None,
            kind,
        }),
        trace: None,
        batch_end,
    }
}

/// Queues one batch and waits until the store publishes it.
async fn publish_records(
    store: &Arc<dyn TimelineStore>,
    key: TimelineKey,
    records: Vec<StoredEvent>,
) {
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
    tokio::time::timeout(PUBLISHED, receiver)
        .await
        .expect("the writer did not publish the write")
        .unwrap()
        .unwrap();
}

fn marked(data: serde_json::Value) -> EventKind {
    EventKind::Extension(ExtensionEvent::new("test", "marked", data).unwrap())
}

fn delta(text: &str) -> EventKind {
    EventKind::Text(DeltaData {
        turn_id: "turn".into(),
        request_id: "request".into(),
        delta: text.into(),
    })
}

/// The same facts in `store`: `main` with two singles, a branch create, one
/// atomic batch of two text deltas and a closing single, and `child` cut at
/// `main`'s second fact.
async fn query_histories(store: Arc<dyn TimelineStore>) -> (Timeline, Timeline) {
    store
        .create_session("query", &SessionConfig::default())
        .await
        .unwrap();
    let main = key("query", "main");
    publish_records(
        &store,
        main.clone(),
        vec![query_record(
            1,
            1,
            "main-1",
            EventKind::InputMessage(InputMessageData::text("one", None)),
        )],
    )
    .await;
    publish_records(
        &store,
        main.clone(),
        vec![query_record(
            2,
            2,
            "main-2",
            marked(serde_json::json!({"tool": "search", "nested": {"level": 3}})),
        )],
    )
    .await;
    publish_records(
        &store,
        main.clone(),
        vec![query_record(
            3,
            3,
            "main-3",
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Sequence { sequence: 2 },
                source: BranchSource::Host {
                    operation_id: "query".into(),
                },
                branch: "child".into(),
                prompt: "continue".into(),
            }),
        )],
    )
    .await;
    publish_records(
        &store,
        main.clone(),
        vec![
            query_record(4, 5, "main-4", delta("a")),
            query_record(5, 5, "main-5", delta("b")),
        ],
    )
    .await;
    publish_records(
        &store,
        main,
        vec![query_record(
            6,
            6,
            "main-6",
            EventKind::StopInterrupt(StopInterruptData {
                reason: "stop".into(),
                scope: InterruptScope::Turn,
            }),
        )],
    )
    .await;
    let child = key("query", "child");
    publish_records(
        &store,
        child.clone(),
        vec![query_record(
            1,
            1,
            "child-1",
            marked(serde_json::json!({"tool": "search"})),
        )],
    )
    .await;
    publish_records(
        &store,
        child,
        vec![
            query_record(
                2,
                3,
                "child-2",
                EventKind::InputMessage(InputMessageData::text("two", None)),
            ),
            query_record(
                3,
                3,
                "child-3",
                EventKind::InputMessage(InputMessageData::text("three", None)),
            ),
        ],
    )
    .await;
    let ctx = rutis::Ctx::root().unwrap();
    let child = Timeline::restore(
        ctx.clone(),
        key("query", "child"),
        vec![("main".into(), 2)],
        store.clone(),
    )
    .await
    .unwrap();
    let main = Timeline::restore(ctx, key("query", "main"), vec![], store)
        .await
        .unwrap();
    (main, child)
}

async fn tall_chain_histories(
    store: Arc<dyn TimelineStore>,
    child_facts: u64,
) -> (Timeline, Timeline) {
    store
        .create_session("tall", &SessionConfig::default())
        .await
        .unwrap();
    let main = key("tall", "main");
    for sequence in 1..=4u64 {
        publish_records(
            &store,
            main.clone(),
            vec![query_record(
                sequence,
                sequence,
                &format!("main-{sequence}"),
                EventKind::InputMessage(InputMessageData::text("fact", None)),
            )],
        )
        .await;
    }
    publish_records(
        &store,
        main.clone(),
        vec![query_record(
            5,
            5,
            "main-5",
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Sequence { sequence: 5 },
                source: BranchSource::Host {
                    operation_id: "tall".into(),
                },
                branch: "child".into(),
                prompt: "continue".into(),
            }),
        )],
    )
    .await;
    let child = key("tall", "child");
    for sequence in 1..=child_facts {
        publish_records(
            &store,
            child.clone(),
            vec![query_record(
                sequence,
                sequence,
                &format!("child-{sequence}"),
                EventKind::InputMessage(InputMessageData::text("fact", None)),
            )],
        )
        .await;
    }
    let ctx = rutis::Ctx::root().unwrap();
    let child = Timeline::restore(ctx.clone(), child, vec![("main".into(), 5)], store.clone())
        .await
        .unwrap();
    let main = Timeline::restore(ctx, main, vec![], store).await.unwrap();
    (main, child)
}

async fn interleaved_histories(store: Arc<dyn TimelineStore>) -> Timeline {
    store
        .create_session("mixed", &SessionConfig::default())
        .await
        .unwrap();
    let main = key("mixed", "main");
    let kinds = [
        EventKind::TurnStart(TurnStartData {
            turn_id: "turn".into(),
        }),
        EventKind::ModelStart(StepData {
            turn_id: "turn".into(),
            request_id: "request".into(),
        }),
        EventKind::TurnStart(TurnStartData {
            turn_id: "turn".into(),
        }),
        EventKind::ModelStart(StepData {
            turn_id: "turn".into(),
            request_id: "request".into(),
        }),
    ];
    for (index, kind) in kinds.into_iter().enumerate() {
        let sequence = index as u64 + 1;
        publish_records(
            &store,
            main.clone(),
            vec![query_record(
                sequence,
                sequence,
                &format!("mixed-{sequence}"),
                kind,
            )],
        )
        .await;
    }
    Timeline::restore(rutis::Ctx::root().unwrap(), main, vec![], store)
        .await
        .unwrap()
}

#[path = "storage_flow/position_queries.rs"]
mod position_queries;

#[tokio::test]
async fn main_and_branch_formats_upgrade_to_v41_without_reinterpreting_their_markers() {
    for (version, main_layout) in [
        (37, true),
        (38, true),
        (37, false),
        (38, false),
        (39, false),
        (40, false),
    ] {
        let database = Database::new();
        let connection = Connection::open(&database.0).unwrap();
        let mut statements = schema_of(if main_layout { 36 } else { version });
        statements.sort_by_key(|sql| {
            !sql.starts_with("CREATE TABLE ") && !sql.starts_with("CREATE VIRTUAL TABLE ")
        });
        for mut sql in statements {
            if sql.starts_with("CREATE TABLE agent_store_format ") {
                sql = format!(
                    "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = {version}))"
                );
            } else if main_layout && version == 38 {
                sql = sql.replace(SESSION_OWNERS_V37, SESSION_OWNERS_V38);
            }
            connection.execute_batch(&sql).unwrap();
        }
        connection
            .execute("INSERT INTO agent_store_format VALUES (?1)", [version])
            .unwrap();
        insert_documented_records(&connection, BUSINESS_TABLES);
        write_documented_context_switches(&connection);
        if main_layout && version == 38 {
            connection
                .execute("UPDATE session_owners SET protocol_version=4", [])
                .unwrap();
        }
        if !main_layout {
            for id in ["s1", "s2"] {
                dim_app_server::storage::initialize_session_member_index(&connection, id).unwrap();
                if version >= 40 {
                    dim_app_server::storage::initialize_session_output_id_index(&connection, id)
                        .unwrap();
                }
            }
        }
        if !main_layout && version < 40 {
            for table in [
                "timeline_member_roots",
                "timeline_member_origins",
                "timeline_member_terminals",
            ] {
                connection.execute(&format!("DELETE FROM {table} WHERE index_id NOT IN ('input/v1','tool/v1','exec/v1','collaboration/v1')"),[]).unwrap();
            }
        }
        let facts = rows(&connection, "timeline_events");
        let owners = rows(&connection, "session_owners");
        drop(connection);
        let mut before = Vec::new();
        dim_app_server::storage::export_timeline(database.0.clone(), "s1", &mut before)
            .await
            .unwrap();
        let connection = Connection::open(&database.0).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT version FROM agent_store_format", [], |row| row
                    .get::<_, u32>(0))
                .unwrap(),
            version
        );
        drop(connection);
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        assert_eq!(db.upgraded_from(), Some(version));
        let store = SqliteTimelineStore::new(db.clone());
        assert_eq!(store.branches("s1").await.unwrap().len(), 2);
        assert_eq!(
            store.read(&key("s1", "main"), 1..=7).await.unwrap().len(),
            7
        );
        let connection = Connection::open(&database.0).unwrap();
        assert_eq!(
            rows(&connection, "timeline_events"),
            facts,
            "facts changed: v{version}, main={main_layout}"
        );
        assert_eq!(rows(&connection, "session_owners"), owners);
        assert_eq!(
            connection
                .query_row("SELECT version FROM agent_store_format", [], |row| row
                    .get::<_, u32>(0))
                .unwrap(),
            STORE_FORMAT
        );
        connection
            .execute("UPDATE session_owners SET protocol_version=4", [])
            .unwrap();
        assert!(
            connection
                .prepare("SELECT * FROM timeline_member_roots")
                .is_ok()
        );
        assert!(
            connection
                .prepare("SELECT * FROM timeline_array_elements")
                .is_ok()
        );
        assert!(
            connection
                .prepare("SELECT * FROM timeline_output_ids")
                .is_ok()
        );
        drop(connection);
        store.close().await.unwrap();
        db.close().await.unwrap();
        let mut after = Vec::new();
        dim_app_server::storage::export_timeline(database.0.clone(), "s1", &mut after)
            .await
            .unwrap();
        assert_eq!(
            before.split(|b| *b == b'\n').skip(1).collect::<Vec<_>>(),
            after.split(|b| *b == b'\n').skip(1).collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_failed_main_format_upgrade_rolls_back_all_added_structures_and_the_marker() {
    for version in [37, 38] {
        let database = Database::new();
        let connection = Connection::open(&database.0).unwrap();
        let mut statements = schema_of(36);
        statements.sort_by_key(|sql| {
            !sql.starts_with("CREATE TABLE ") && !sql.starts_with("CREATE VIRTUAL TABLE ")
        });
        for mut sql in statements {
            if sql.starts_with("CREATE TABLE agent_store_format ") {
                sql = format!(
                    "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = {version}))"
                );
            } else if version == 38 {
                sql = sql.replace(SESSION_OWNERS_V37, SESSION_OWNERS_V38);
            }
            connection.execute_batch(&sql).unwrap();
        }
        connection
            .execute("INSERT INTO agent_store_format VALUES (?1)", [version])
            .unwrap();
        insert_documented_records(&connection, BUSINESS_TABLES);
        write_documented_context_switches(&connection);
        connection
            .execute(
                "UPDATE timeline_events SET body='{}' WHERE session_id='s1' AND sequence=2",
                [],
            )
            .unwrap();
        let before = schema(&connection);
        let facts = rows(&connection, "timeline_events");
        drop(connection);
        assert!(SqliteStorage::open(database.0.clone()).is_err());
        let connection = Connection::open(&database.0).unwrap();
        assert_eq!(schema(&connection), before);
        assert_eq!(rows(&connection, "timeline_events"), facts);
        assert_eq!(
            connection
                .query_row("SELECT version FROM agent_store_format", [], |row| row
                    .get::<_, u32>(0))
                .unwrap(),
            version
        );
    }
}

use dim_agent_runtime::{AgentError, Result, panic_message};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use rutis::host::ErrorKind;
use rutis::{Ctx, Plugin};
use std::{
    fs::File,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

/// The native store format this daemon creates and opens.
pub const FORMAT: u32 = 34;
/// The oldest schema the startup transaction upgrades: the first step of
/// `UPGRADES`, which is ordered oldest first.
const MIN_FORMAT: u32 = UPGRADES[0].0;
/// WAL bytes after which the writer checkpoints, outside its commit. This is
/// SQLite's own automatic threshold (1000 pages of 4 KiB) moved off the
/// commit path.
const CHECKPOINT_BYTES: u64 = 4 * 1024 * 1024;
/// The WAL file is truncated to this size when a checkpoint restarts it.
const WAL_LIMIT_BYTES: i64 = 64 * 1024 * 1024;
/// The process connection's page cache in KiB. A measurement over a 650 MB
/// store with 2,300 Sessions put their hot tail pages (about 20 MB) in cache
/// from 32 MiB on; larger caches read no fewer bytes.
const PAGE_CACHE_KIB: i64 = 32 * 1024;
/// Indexes a format range had and the current schema no longer creates, with
/// the first and last format that had them.
const LEGACY_INDEXES: &[(&str, &str, u32, u32)] = &[
    (
        "timeline_events_by_end",
        "CREATE INDEX timeline_events_by_end ON timeline_events(session_id, branch_id, last_sequence, sequence)",
        13,
        24,
    ),
    (
        "timeline_events_render",
        "CREATE INDEX timeline_events_render ON timeline_events(session_id, branch_id, sequence) WHERE context_render = 1",
        31,
        31,
    ),
    (
        "timeline_events_state",
        "CREATE INDEX timeline_events_state ON timeline_events(session_id, branch_id, sequence) WHERE context_state = 1",
        31,
        31,
    ),
];
pub(super) const SCHEMA: &[(&str, &str)] = &[
    (
        "agent_store_format",
        "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 34))",
    ),
    (
        "settings_documents",
        "CREATE TABLE settings_documents (source TEXT NOT NULL PRIMARY KEY, version INTEGER NOT NULL CHECK(version > 0), document TEXT NOT NULL, imported_revision TEXT NOT NULL)",
    ),
    (
        "provider_imports",
        "CREATE TABLE provider_imports (kind TEXT NOT NULL, id TEXT NOT NULL, source_schema TEXT NOT NULL, source_version INTEGER NOT NULL, source_digest TEXT NOT NULL, PRIMARY KEY (kind, id))",
    ),
    (
        "providers",
        "CREATE TABLE providers (provider_id TEXT NOT NULL PRIMARY KEY, version INTEGER NOT NULL CHECK(version > 0), record TEXT NOT NULL)",
    ),
    (
        "provider_selections",
        "CREATE TABLE provider_selections (scope TEXT NOT NULL PRIMARY KEY, version INTEGER NOT NULL CHECK(version > 0), provider_id TEXT NOT NULL, model_id TEXT NOT NULL)",
    ),
    (
        "agent_sessions",
        "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0), leader_session_id TEXT REFERENCES agent_sessions(session_id), origin TEXT NOT NULL DEFAULT 'unknown' CHECK (origin IN ('client', 'acp', 'internal', 'unknown')))",
    ),
    (
        "agent_sessions_by_leader",
        "CREATE INDEX agent_sessions_by_leader ON agent_sessions(leader_session_id)",
    ),
    (
        "agents",
        "CREATE TABLE agents(agent_id TEXT PRIMARY KEY, profile TEXT NOT NULL, is_default INTEGER NOT NULL CHECK(is_default IN (0, 1)), owner_session_id TEXT REFERENCES agent_sessions(session_id))",
    ),
    (
        "agents_default_unique",
        "CREATE UNIQUE INDEX agents_default_unique ON agents(is_default) WHERE is_default = 1",
    ),
    (
        "agents_by_owner",
        "CREATE INDEX agents_by_owner ON agents(owner_session_id)",
    ),
    (
        "session_owners",
        "CREATE TABLE session_owners(session_id TEXT PRIMARY KEY REFERENCES agent_sessions(session_id), agent_id TEXT NOT NULL REFERENCES agents(agent_id), protocol_version INTEGER NOT NULL CHECK(protocol_version IN (1, 2, 3)))",
    ),
    (
        "timeline_branches",
        "CREATE TABLE timeline_branches (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, info TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence >= 0), PRIMARY KEY (session_id, branch_id), FOREIGN KEY (session_id) REFERENCES agent_sessions(session_id))",
    ),
    (
        "timeline_content",
        "CREATE TABLE timeline_content (session_id TEXT NOT NULL, hash TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY (session_id, hash), FOREIGN KEY (session_id) REFERENCES agent_sessions(session_id))",
    ),
    (
        "timeline_events",
        "CREATE TABLE timeline_events (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), last_sequence INTEGER NOT NULL CHECK (last_sequence >= sequence), kind TEXT NOT NULL, body TEXT NOT NULL, event_id TEXT, context_render INTEGER NOT NULL DEFAULT 0, context_state INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (session_id, branch_id, sequence), FOREIGN KEY (session_id, branch_id) REFERENCES timeline_branches(session_id, branch_id))",
    ),
    (
        "timeline_snapshots",
        "CREATE TABLE timeline_snapshots (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), interpreter TEXT NOT NULL, event_id TEXT NOT NULL, state TEXT NOT NULL, PRIMARY KEY (session_id, branch_id, interpreter, sequence), FOREIGN KEY (session_id, branch_id, sequence) REFERENCES timeline_events(session_id, branch_id, sequence))",
    ),
    (
        "session_owners_by_agent",
        "CREATE INDEX session_owners_by_agent ON session_owners(agent_id, protocol_version, session_id)",
    ),
    (
        "timeline_events_by_id",
        "CREATE INDEX timeline_events_by_id ON timeline_events(session_id, event_id) WHERE event_id IS NOT NULL",
    ),
    (
        "timeline_events_context",
        "CREATE INDEX timeline_events_context ON timeline_events(session_id, branch_id, sequence) WHERE context_render = 1 OR context_state = 1",
    ),
    (
        "timeline_events_positions",
        "CREATE INDEX timeline_events_positions ON timeline_events(session_id, branch_id, kind, sequence) WHERE kind IN ('turn/start', 'context/compaction-start', 'context/compaction')",
    ),
    (
        "timeline_tool_use_starts",
        "CREATE INDEX timeline_tool_use_starts ON timeline_events(session_id, json_extract(body, '$.record.event.data.request_id'), json_extract(body, '$.record.event.data.call_id')) WHERE branch_id = 'main' AND kind = 'step/tool-use-start'",
    ),
    (
        "session_files",
        "CREATE TABLE session_files (file_id TEXT NOT NULL PRIMARY KEY, node TEXT, writer TEXT)",
    ),
    (
        "session_file_owners",
        "CREATE TABLE session_file_owners (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), file_id TEXT NOT NULL REFERENCES session_files(file_id), PRIMARY KEY (session_id, file_id))",
    ),
    (
        "session_file_owners_by_file",
        "CREATE INDEX session_file_owners_by_file ON session_file_owners(file_id)",
    ),
    (
        "session_grants",
        "CREATE TABLE session_grants (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), path TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('file', 'dir')), mode TEXT NOT NULL CHECK (mode IN ('read', 'write')), PRIMARY KEY (session_id, path))",
    ),
    (
        "session_output_owners",
        "CREATE TABLE session_output_owners (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), path TEXT NOT NULL, PRIMARY KEY (session_id, path))",
    ),
    (
        "session_output_owners_by_path",
        "CREATE INDEX session_output_owners_by_path ON session_output_owners(path)",
    ),
    (
        "worktree_directories",
        "CREATE TABLE worktree_directories (repository TEXT NOT NULL, root TEXT NOT NULL, PRIMARY KEY (repository, root))",
    ),
    (
        "session_worktrees",
        "CREATE TABLE session_worktrees (session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), repository TEXT NOT NULL, root TEXT NOT NULL, subtree TEXT NOT NULL, PRIMARY KEY (session_id, repository, root), FOREIGN KEY (repository, root) REFERENCES worktree_directories(repository, root))",
    ),
    (
        "session_worktrees_by_directory",
        "CREATE INDEX session_worktrees_by_directory ON session_worktrees(repository, root, session_id)",
    ),
    (
        "session_metadata",
        "CREATE TABLE session_metadata (session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id), revision INTEGER NOT NULL CHECK (revision > 0), title TEXT, title_source TEXT CHECK (title_source IN ('auto', 'manual')), CHECK ((title IS NULL) = (title_source IS NULL)))",
    ),
    (
        "session_catalog",
        "CREATE TABLE session_catalog (session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id), updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0), preview TEXT, cwd TEXT NOT NULL)",
    ),
    (
        "session_catalog_by_activity",
        "CREATE INDEX session_catalog_by_activity ON session_catalog(updated_at_ms DESC, session_id)",
    ),
    (
        "session_catalog_with_preview",
        "CREATE INDEX session_catalog_with_preview ON session_catalog(updated_at_ms DESC, session_id) WHERE preview IS NOT NULL",
    ),
];
/// The text of a table before v27 adds a column to it; `schema_of` uses it
/// to recognize a store older than v27.
const AGENT_SESSIONS_BEFORE_V27: &str = "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0))";
const AGENTS_BEFORE_V27: &str = "CREATE TABLE agents(agent_id TEXT PRIMARY KEY, profile TEXT NOT NULL, is_default INTEGER NOT NULL CHECK(is_default IN (0, 1)))";
/// The text of `agent_sessions` from v27 through v29; `schema_of` uses it to
/// recognize a store older than v30.
const AGENT_SESSIONS_BEFORE_V30: &str = "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0), leader_session_id TEXT REFERENCES agent_sessions(session_id))";
/// The text of `timeline_events` before v31 adds the two reading switches.
const TIMELINE_EVENTS_BEFORE_V31: &str = "CREATE TABLE timeline_events (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), last_sequence INTEGER NOT NULL CHECK (last_sequence >= sequence), kind TEXT NOT NULL, body TEXT NOT NULL, event_id TEXT, PRIMARY KEY (session_id, branch_id, sequence), FOREIGN KEY (session_id, branch_id) REFERENCES timeline_branches(session_id, branch_id))";
/// v27 adds a column to two tables, which `SCHEMA` cannot express as a new
/// entry: the upgrade alters them before it creates the indexes on them.
const V27_COLUMNS: &str = "ALTER TABLE agent_sessions ADD COLUMN leader_session_id TEXT REFERENCES agent_sessions(session_id);
ALTER TABLE agents ADD COLUMN owner_session_id TEXT REFERENCES agent_sessions(session_id);";
/// v30 adds one column to `agent_sessions` and backfills the rows whose leader
/// link already records an internal Session. The `ALTER` text must reproduce
/// `SCHEMA`'s column definition exactly: the startup gate compares the stored
/// statement character by character.
const V30_COLUMNS: &str = "ALTER TABLE agent_sessions ADD COLUMN origin TEXT NOT NULL DEFAULT 'unknown' CHECK (origin IN ('client', 'acp', 'internal', 'unknown'));
UPDATE agent_sessions SET origin='internal' WHERE leader_session_id IS NOT NULL;";
/// v31 adds the two reading switches; the upgrade alters the table before it
/// creates the partial indexes on them.
const V31_COLUMNS: &str =
    "ALTER TABLE timeline_events ADD COLUMN context_render INTEGER NOT NULL DEFAULT 0;
ALTER TABLE timeline_events ADD COLUMN context_state INTEGER NOT NULL DEFAULT 0;";
/// The one-way upgrade steps, oldest first: the format a step starts from
/// and the `SCHEMA` entries it adds, which are created in `SCHEMA` order,
/// tables before their indexes. An upgrade also replaces the marker table.
const UPGRADES: [(u32, &[&str]); 20] = [
    (
        13,
        &[
            "session_files",
            "session_file_owners",
            "session_file_owners_by_file",
        ],
    ),
    (15, &["session_metadata"]),
    // #193: request messages as lists of `node` content rows; the marker
    // only.
    (16, &[]),
    // #2: extension event rows, which a v17 daemon cannot decode; the
    // marker only.
    (17, &[]),
    // #364: Session configurations and Agent profiles without a mount list
    // omit `tools`, which a v18 daemon cannot decode; the marker only.
    (18, &[]),
    (19, &[]),
    // #527: a tool result's `effects` and a completed compaction's
    // `digest_entries`, which a v20 daemon cannot decode; the marker only.
    (20, &[]),
    // #580: typeless tool schemas, unreadable by v21.
    (21, &[]),
    (
        22,
        &[
            "session_catalog",
            "session_catalog_by_activity",
            "session_catalog_with_preview",
        ],
    ),
    // #555: a configuration's `system_source`, which a v23 daemon cannot
    // decode; the marker only.
    (23, &[]),
    (24, &[]),
    // #522: model limits that only a declaration writes, which a v25
    // daemon cannot decode; the marker only, and the Provider-owned
    // rewriter runs at this step.
    (25, &[]),
    // #593: a delegation's owner and a member Session's leader.
    (26, &["agents_by_owner", "agent_sessions_by_leader"]),
    (27, &["session_grants"]),
    // ADR 013: output ownership, copied by a fork and read by the
    // `session-owned` fact.
    (
        28,
        &["session_output_owners", "session_output_owners_by_path"],
    ),
    // #557: a Session's creation origin; the column only, and the step fills
    // in the rows whose leader link already records one.
    (29, &[]),
    // #708: the two reading switches on timeline events; v32 keeps the
    // switch columns and the position index, and replaces the two
    // reading-switch indexes with one.
    (30, &["timeline_events_positions"]),
    // #605: one partial index over both reading switches replaces the two.
    (31, &["timeline_events_context"]),
    (
        32,
        &[
            "worktree_directories",
            "session_worktrees",
            "session_worktrees_by_directory",
        ],
    ),
    // #1099: a Session's activity time is the newest activity event on
    // `main`, and the step recomputes the derived `session_catalog`
    // column; the marker only.
    (33, &[]),
];

fn format_table(version: u32) -> String {
    format!(
        "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = {version}))"
    )
}
/// The exact schema of a format this daemon opens or upgrades: `SCHEMA`
/// without later additions, with its marker and the index removed after v24.
fn schema_of(version: u32) -> Vec<(&'static str, String)> {
    SCHEMA
        .iter()
        .copied()
        .chain(
            LEGACY_INDEXES
                .iter()
                .filter(|(_, _, from, through)| *from <= version && version <= *through)
                .map(|&(name, sql, _, _)| (name, sql)),
        )
        .filter(|(name, _)| {
            !UPGRADES
                .iter()
                .any(|(from, added)| *from >= version && added.contains(name))
        })
        .map(|(name, sql)| {
            let sql = match name {
                "agent_store_format" => format_table(version),
                "agent_sessions" if version < 27 => AGENT_SESSIONS_BEFORE_V27.to_owned(),
                "agent_sessions" if version < 30 => AGENT_SESSIONS_BEFORE_V30.to_owned(),
                "agents" if version < 27 => AGENTS_BEFORE_V27.to_owned(),
                "timeline_events" if version < 31 => TIMELINE_EVENTS_BEFORE_V31.to_owned(),
                _ => sql.to_owned(),
            };
            (name, sql)
        })
        .collect()
}

/// Whether `actual`, sorted by name, is exactly `expected`.
fn same_schema(actual: &[(String, String)], mut expected: Vec<(&str, String)>) -> bool {
    expected.sort_unstable();
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|((name, sql), (expected_name, expected_sql))| {
                name == expected_name && *sql == expected_sql
            })
}
/// The read indexes the daemon creates at open: the latest-turn window's
/// ([session-history-window §5.1](../../../../docs/design/appserver/session-history-window.md#51-the-derived-index)),
/// the policy window's turn starts, and the event query's by kind, which carries
/// the event id so a kind read needs no table body for it (ADR 019 D4). They
/// carry no data and no decode rule, so they are not part of the format: `schema`
/// and the format gate ignore them, and every open recreates them.
const DERIVED_INDEXES: &[(&str, &str)] = &[
    (
        "timeline_history_anchors",
        "CREATE INDEX IF NOT EXISTS timeline_history_anchors ON timeline_events(session_id, branch_id, kind, sequence) WHERE kind IN ('turn/start', 'input/message')",
    ),
    (
        "timeline_events_by_kind",
        "CREATE INDEX IF NOT EXISTS timeline_events_by_kind ON timeline_events(session_id, branch_id, kind, sequence, event_id)",
    ),
    (
        "timeline_policy_turns",
        "CREATE INDEX IF NOT EXISTS timeline_policy_turns ON timeline_events(session_id, branch_id, kind, sequence, event_id) WHERE kind = 'turn/start'",
    ),
    (
        "timeline_submissions",
        "CREATE TABLE IF NOT EXISTS timeline_submissions (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, submission_id TEXT NOT NULL, event_id TEXT NOT NULL, sequence INTEGER NOT NULL, PRIMARY KEY (session_id, branch_id, submission_id, event_id))",
    ),
    (
        "timeline_submissions_ai",
        "CREATE TRIGGER IF NOT EXISTS timeline_submissions_ai AFTER INSERT ON timeline_events BEGIN \
           INSERT OR REPLACE INTO timeline_submissions(session_id, branch_id, submission_id, event_id, sequence) \
           SELECT NEW.session_id, NEW.branch_id, json_extract(NEW.body, '$.record.event.data.submission_id'), NEW.event_id, NEW.sequence \
           WHERE NEW.kind = 'input/message' AND NEW.event_id IS NOT NULL AND json_valid(NEW.body) \
             AND json_extract(NEW.body, '$.record.event.data.submission_id') IS NOT NULL; \
         END",
    ),
    (
        "timeline_submissions_au",
        "CREATE TRIGGER IF NOT EXISTS timeline_submissions_au AFTER UPDATE ON timeline_events BEGIN \
           DELETE FROM timeline_submissions WHERE session_id = OLD.session_id AND branch_id = OLD.branch_id AND event_id = OLD.event_id; \
           INSERT OR REPLACE INTO timeline_submissions(session_id, branch_id, submission_id, event_id, sequence) \
           SELECT NEW.session_id, NEW.branch_id, json_extract(NEW.body, '$.record.event.data.submission_id'), NEW.event_id, NEW.sequence \
           WHERE NEW.kind = 'input/message' AND NEW.event_id IS NOT NULL AND json_valid(NEW.body) \
             AND json_extract(NEW.body, '$.record.event.data.submission_id') IS NOT NULL; \
         END",
    ),
    (
        "timeline_submissions_ad",
        "CREATE TRIGGER IF NOT EXISTS timeline_submissions_ad AFTER DELETE ON timeline_events BEGIN \
           DELETE FROM timeline_submissions WHERE session_id = OLD.session_id AND branch_id = OLD.branch_id AND event_id = OLD.event_id; \
         END",
    ),
    (
        "timeline_key_blocks",
        "CREATE TABLE IF NOT EXISTS timeline_key_blocks (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, key TEXT NOT NULL, sequence INTEGER NOT NULL, block_index INTEGER NOT NULL, input_id TEXT NOT NULL, text TEXT NOT NULL, retire INTEGER NOT NULL, event_id TEXT NOT NULL, PRIMARY KEY (session_id, branch_id, event_id, block_index))",
    ),
    (
        "timeline_deleted_inputs",
        "CREATE TABLE IF NOT EXISTS timeline_deleted_inputs (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, input_id TEXT NOT NULL, sequence INTEGER NOT NULL, event_id TEXT NOT NULL, PRIMARY KEY (session_id, branch_id, event_id))",
    ),
    (
        "timeline_openings",
        "CREATE TABLE IF NOT EXISTS timeline_openings (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL, event_id TEXT NOT NULL, PRIMARY KEY (session_id, branch_id, event_id))",
    ),
    (
        "timeline_key_blocks_ai",
        "CREATE TRIGGER IF NOT EXISTS timeline_key_blocks_ai AFTER INSERT ON timeline_events BEGIN \
           INSERT OR REPLACE INTO timeline_key_blocks(session_id, branch_id, key, sequence, block_index, input_id, text, retire, event_id) \
           SELECT NEW.session_id, NEW.branch_id, json_extract(j.value, '$.key'), NEW.sequence, CAST(j.key AS INTEGER), NEW.event_id, json_extract(j.value, '$.text'), COALESCE(json_extract(j.value, '$.retire'), 0), NEW.event_id \
           FROM json_each(json_extract(NEW.body, '$.record.event.data.blocks')) AS j \
           WHERE NEW.kind = 'input/message' AND json_valid(NEW.body) \
             AND json_type(j.value, '$.key') = 'text' AND json_type(j.value, '$.text') = 'text'; \
           INSERT OR REPLACE INTO timeline_deleted_inputs(session_id, branch_id, input_id, sequence, event_id) \
           SELECT NEW.session_id, NEW.branch_id, json_extract(NEW.body, '$.record.event.data.input_id'), NEW.sequence, NEW.event_id \
           WHERE NEW.kind = 'input/deleted' AND json_valid(NEW.body) \
             AND json_extract(NEW.body, '$.record.event.data.input_id') IS NOT NULL; \
           INSERT OR REPLACE INTO timeline_openings(session_id, branch_id, sequence, event_id) \
           SELECT NEW.session_id, NEW.branch_id, NEW.sequence, NEW.event_id \
           WHERE NEW.kind IN ('branch/start', 'session/forked'); \
         END",
    ),
    (
        "timeline_key_blocks_au",
        "CREATE TRIGGER IF NOT EXISTS timeline_key_blocks_au AFTER UPDATE ON timeline_events BEGIN \
           DELETE FROM timeline_key_blocks WHERE session_id = OLD.session_id AND branch_id = OLD.branch_id AND event_id = OLD.event_id; \
           DELETE FROM timeline_deleted_inputs WHERE session_id = OLD.session_id AND branch_id = OLD.branch_id AND event_id = OLD.event_id; \
           DELETE FROM timeline_openings WHERE session_id = OLD.session_id AND branch_id = OLD.branch_id AND event_id = OLD.event_id; \
           INSERT OR REPLACE INTO timeline_key_blocks(session_id, branch_id, key, sequence, block_index, input_id, text, retire, event_id) \
           SELECT NEW.session_id, NEW.branch_id, json_extract(j.value, '$.key'), NEW.sequence, CAST(j.key AS INTEGER), NEW.event_id, json_extract(j.value, '$.text'), COALESCE(json_extract(j.value, '$.retire'), 0), NEW.event_id \
           FROM json_each(json_extract(NEW.body, '$.record.event.data.blocks')) AS j \
           WHERE NEW.kind = 'input/message' AND json_valid(NEW.body) \
             AND json_type(j.value, '$.key') = 'text' AND json_type(j.value, '$.text') = 'text'; \
           INSERT OR REPLACE INTO timeline_deleted_inputs(session_id, branch_id, input_id, sequence, event_id) \
           SELECT NEW.session_id, NEW.branch_id, json_extract(NEW.body, '$.record.event.data.input_id'), NEW.sequence, NEW.event_id \
           WHERE NEW.kind = 'input/deleted' AND json_valid(NEW.body) \
             AND json_extract(NEW.body, '$.record.event.data.input_id') IS NOT NULL; \
           INSERT OR REPLACE INTO timeline_openings(session_id, branch_id, sequence, event_id) \
           SELECT NEW.session_id, NEW.branch_id, NEW.sequence, NEW.event_id \
           WHERE NEW.kind IN ('branch/start', 'session/forked'); \
         END",
    ),
    (
        "timeline_key_blocks_ad",
        "CREATE TRIGGER IF NOT EXISTS timeline_key_blocks_ad AFTER DELETE ON timeline_events BEGIN \
           DELETE FROM timeline_key_blocks WHERE session_id = OLD.session_id AND branch_id = OLD.branch_id AND event_id = OLD.event_id; \
           DELETE FROM timeline_deleted_inputs WHERE session_id = OLD.session_id AND branch_id = OLD.branch_id AND event_id = OLD.event_id; \
           DELETE FROM timeline_openings WHERE session_id = OLD.session_id AND branch_id = OLD.branch_id AND event_id = OLD.event_id; \
         END",
    ),
];
/// The schema objects of `connection`'s database that belong to the format,
/// sorted by name.
fn schema(connection: &Connection) -> Result<Vec<(String, String)>> {
    let derived = DERIVED_INDEXES
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    let placeholders = vec!["?"; derived.len()].join(",");
    let mut query = connection
        .prepare(&format!(
            "SELECT name, sql FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_%' AND name NOT LIKE 'timeline_submissions%' AND name NOT IN ({placeholders}) ORDER BY name"
        ))
        .map_err(storage)?;
    let objects = query
        .query_map(rusqlite::params_from_iter(derived), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)?;
    Ok(objects
        .into_iter()
        .filter(|(name, _): &(String, String)| {
            !DERIVED_INDEXES.iter().any(|(derived, _)| derived == name)
        })
        .collect())
}
/// The format whose exact schema `actual` is: the current format, or one an
/// upgrade step starts from.
fn known_format(actual: &[(String, String)]) -> Option<u32> {
    std::iter::once(FORMAT)
        .chain(UPGRADES.iter().map(|(from, _)| *from))
        .find(|version| same_schema(actual, schema_of(*version)))
}
/// The rows of the format marker table.
fn markers(connection: &Connection) -> Result<Vec<i64>> {
    connection
        .prepare("SELECT version FROM agent_store_format")
        .map_err(storage)?
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)
}

fn storage(error: impl std::fmt::Display) -> AgentError {
    AgentError::Storage(error.to_string())
}

enum StartupFailure {
    Newer { marker: i64 },
    Mismatched { schema: u32, marker: Option<i64> },
    Unrecognized { marker: Option<i64> },
    TooOld { marker: i64 },
}
impl StartupFailure {
    /// The note a CLI-started daemon leaves for its client; the first line
    /// binds it to the process that wrote it.
    fn note(&self, path: &std::path::Path) -> String {
        let path = path.display();
        let body = match self {
            Self::Newer { marker } => format!(
                "Cannot open native store {path}.\n  Store format: v{marker}\n  Daemon format: v{FORMAT}\n  This daemon cannot read the store. Existing data was preserved.\n  Run a build supporting v{marker}, or use --home PATH for a separate environment."
            ),
            Self::Mismatched { schema, marker } => {
                let store = match marker {
                    Some(marker) => format!("v{marker} (schema is v{schema})"),
                    None => format!("v{schema} (format marker is missing or ambiguous)"),
                };
                format!(
                    "Cannot open native store {path}.\n  Store format: {store}\n  Daemon format: v{FORMAT}\n  The store's format marker and schema disagree. Existing data was preserved.\n  Use the build that wrote this store, or use --home PATH for a separate environment."
                )
            }
            Self::Unrecognized { marker } => {
                let store = match marker {
                    Some(marker) => format!("v{marker} (unrecognized schema)"),
                    None => "unrecognized".to_owned(),
                };
                format!(
                    "Cannot open native store {path}.\n  Store format: {store}\n  Daemon format: v{FORMAT}\n  The store is not a format this daemon recognizes. Existing data was preserved.\n  Use the build that wrote this store, or use --home PATH for a separate environment."
                )
            }
            Self::TooOld { marker } => format!(
                "Cannot open native store {path}.\n  Store format: v{marker} (no longer read by this daemon)\n  Daemon format: v{FORMAT}\n  This daemon cannot read the store. Existing data was preserved.\n  Use a build that reads v{marker}, or use --home PATH for a separate environment."
            ),
        };
        format!(
            "dimcode-startup-failure v1 pid={}\n{body}",
            std::process::id()
        )
    }
}

fn startup_note_path(path: &std::path::Path) -> PathBuf {
    let mut note = path.as_os_str().to_os_string();
    note.push(".startup");
    PathBuf::from(note)
}

fn single_marker(markers: &[i64]) -> Option<i64> {
    (markers.len() == 1).then(|| markers[0])
}

/// Best effort: the rejection and its error text do not depend on the note.
fn write_startup_failure(path: &std::path::Path, failure: StartupFailure) {
    let note = failure.note(path);
    let target = startup_note_path(path);
    let temp = target.with_file_name(format!(
        "{}.{}.tmp",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dimcode.sqlite.startup"),
        uuid::Uuid::new_v4()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(note.as_bytes())?;
        drop(file);
        let _ = std::fs::remove_file(&target);
        std::fs::rename(&temp, &target)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

/// The process connection and maintenance admission. Business modules never
/// open a second connection to this database.
pub struct SqliteStorage {
    pub(super) connection: Mutex<Option<Connection>>,
    pub(super) readers: Readers,
    admission: Mutex<Admission>,
    idle: tokio::sync::Notify,
    pub(super) path: PathBuf,
    upgraded_from: Option<u32>,
}
/// The read-only connections that serve reads while the writer keeps its own
/// (#599). Four let the catalog scan, a history page and the settings,
/// provider and Session-file reads run together; reads are short calls, and a
/// fifth connection adds a page cache without a measured gain.
const READ_CONNECTIONS: usize = 4;
pub(super) struct Readers {
    pub(super) connections: Vec<Mutex<Option<Connection>>>,
    next: AtomicUsize,
}
impl Readers {
    /// Opens `READ_CONNECTIONS` read-only connections beside the writer.
    fn open(path: &Path) -> Result<Self> {
        let mut connections = Vec::with_capacity(READ_CONNECTIONS);
        for _ in 0..READ_CONNECTIONS {
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(storage)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(storage)?;
            connection
                .pragma_update(None, "cache_size", -PAGE_CACHE_KIB)
                .map_err(storage)?;
            functions(&connection)?;
            connections.push(Mutex::new(Some(connection)));
        }
        Ok(Self {
            connections,
            next: AtomicUsize::new(0),
        })
    }
    /// The read-only export opens one connection that serves its reads.
    fn none() -> Self {
        Self {
            connections: Vec::new(),
            next: AtomicUsize::new(0),
        }
    }
}
/// SQLite's busy handler for the writer and a copy connection (#599): retry
/// once a millisecond for about five seconds, so one lock holder never waits
/// out the other's backoff while the two take turns on the write lock.
fn busy(retries: i32) -> bool {
    if retries >= 5000 {
        return false;
    }
    std::thread::sleep(Duration::from_millis(1));
    true
}
fn functions(connection: &Connection) -> Result<()> {
    connection
        .create_scalar_function(
            "catalog_fold",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC
                | rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            |ctx| {
                Ok(ctx
                    .get::<Option<String>>(0)?
                    .map(|text| text.to_lowercase()))
            },
        )
        .map_err(storage)
}
struct Admission {
    closed: bool,
    running: usize,
    lease: Option<DatabaseLease>,
}
struct DatabaseLease(File);
impl Drop for DatabaseLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
struct Operation(Arc<SqliteStorage>);
impl Drop for Operation {
    fn drop(&mut self) {
        let mut admission = self.0.admission.lock().unwrap();
        admission.running -= 1;
        if admission.running == 0 {
            self.0.idle.notify_waiters();
        }
    }
}
impl SqliteStorage {
    pub fn open(path: PathBuf) -> Result<Arc<Self>> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent).map_err(storage)?;
        let path = if path.exists() {
            path.canonicalize().map_err(storage)?
        } else {
            parent.canonicalize().map_err(storage)?.join(
                path.file_name()
                    .ok_or_else(|| storage("database path has no file name"))?,
            )
        };
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let admission = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(storage)?;
        admission.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => storage(format!(
                "another process owns the SQLite database: {}",
                path.display()
            )),
            std::fs::TryLockError::Error(error) => storage(error),
        })?;
        let admission = DatabaseLease(admission);
        let mut connection = Connection::open(&path).map_err(storage)?;
        functions(&connection)?;
        connection.busy_handler(Some(busy)).map_err(storage)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let actual = schema(&tx)?;
        let unsupported_version = || {
            storage(format!(
                "unsupported Timeline storage format version at {}; the database was preserved. Use a compatible daemon to access it, or choose a new database path for a fresh start.",
                path.display()
            ))
        };
        let mut upgraded_from = None;
        if actual.is_empty() {
            for (_, sql) in SCHEMA {
                tx.execute_batch(sql).map_err(storage)?;
            }
            tx.execute("INSERT INTO agent_store_format VALUES (?1)", [FORMAT])
                .map_err(storage)?;
        } else if let Some(version) = known_format(&actual) {
            let markers = markers(&tx)?;
            if markers != [i64::from(version)] {
                write_startup_failure(
                    &path,
                    StartupFailure::Mismatched {
                        schema: version,
                        marker: single_marker(&markers),
                    },
                );
                return Err(unsupported_version());
            }
            if version != FORMAT {
                tx.execute_batch("DROP TABLE agent_store_format")
                    .map_err(storage)?;
                tx.execute_batch(&format_table(FORMAT)).map_err(storage)?;
                for (from, added) in UPGRADES.iter().filter(|(from, _)| *from >= version) {
                    if *from == 26 {
                        tx.execute_batch(V27_COLUMNS).map_err(storage)?;
                    }
                    if *from == 29 {
                        tx.execute_batch(V30_COLUMNS).map_err(storage)?;
                    }
                    if *from == 30 {
                        tx.execute_batch(V31_COLUMNS).map_err(storage)?;
                        let keys = |list: &[&str]| {
                            list.iter()
                                .map(|key| format!("'{key}'"))
                                .collect::<Vec<_>>()
                                .join(",")
                        };
                        tx.execute_batch(&format!(
                            "UPDATE timeline_events SET context_render=1 WHERE kind IN ({});",
                            keys(dim_agent_types::CONTEXT_RENDER_KEYS)
                        ))
                        .map_err(storage)?;
                        tx.execute_batch(&format!(
                            "UPDATE timeline_events SET context_state=1 WHERE kind IN ({});",
                            keys(dim_agent_types::CONTEXT_STATE_KEYS)
                        ))
                        .map_err(storage)?;
                    }
                    if *from == 31 && version == 31 {
                        tx.execute_batch(
                            "DROP INDEX timeline_events_render; DROP INDEX timeline_events_state",
                        )
                        .map_err(storage)?;
                    }
                    for (_, sql) in SCHEMA.iter().filter(|(name, _)| added.contains(name)) {
                        tx.execute_batch(sql).map_err(storage)?;
                    }
                    if *from == 22 {
                        super::catalog::backfill(&tx)?;
                    }
                    if *from == 24 {
                        tx.execute_batch("DROP INDEX timeline_events_by_end")
                            .map_err(storage)?;
                    }
                    if *from == 25 {
                        crate::provider::rewrite_stored_limits(&tx)?;
                    }
                    // #1099: only the derived `updated_at_ms` column is
                    // rewritten, by the same backfill the catalog's own
                    // upgrade used.
                    if *from == 33 {
                        super::catalog::backfill(&tx)?;
                    }
                }
                tx.execute("INSERT INTO agent_store_format VALUES (?1)", [FORMAT])
                    .map_err(storage)?;
                upgraded_from = Some(version);
            }
        } else {
            let marker = markers(&tx)
                .ok()
                .and_then(|markers| single_marker(&markers));
            let failure = match marker {
                Some(marker) if marker > i64::from(FORMAT) => StartupFailure::Newer { marker },
                Some(marker) if marker == 14 || marker < i64::from(MIN_FORMAT) => {
                    StartupFailure::TooOld { marker }
                }
                Some(marker) => StartupFailure::Unrecognized {
                    marker: Some(marker),
                },
                None => StartupFailure::Unrecognized { marker: None },
            };
            write_startup_failure(&path, failure);
            return Err(storage(format!(
                "unsupported SQLite schema at {}; the database was preserved. Use a compatible daemon to access it, or choose a new database path for a fresh start.",
                path.display()
            )));
        }
        for (_, sql) in DERIVED_INDEXES {
            tx.execute_batch(sql).map_err(storage)?;
        }
        let submission_rows: i64 = tx
            .query_row("SELECT count(*) FROM timeline_submissions", [], |row| {
                row.get(0)
            })
            .map_err(storage)?;
        if submission_rows == 0 {
            tx.execute_batch(
                "INSERT OR REPLACE INTO timeline_submissions(session_id, branch_id, submission_id, event_id, sequence) \
                 SELECT session_id, branch_id, json_extract(body, '$.record.event.data.submission_id'), event_id, sequence \
                 FROM timeline_events \
                 WHERE kind = 'input/message' AND event_id IS NOT NULL AND json_valid(body) \
                   AND json_extract(body, '$.record.event.data.submission_id') IS NOT NULL",
            )
            .map_err(storage)?;
        }
        let key_rows: i64 = tx
            .query_row("SELECT count(*) FROM timeline_key_blocks", [], |row| {
                row.get(0)
            })
            .map_err(storage)?;
        if key_rows == 0 {
            tx.execute_batch(
                "INSERT OR REPLACE INTO timeline_key_blocks(session_id, branch_id, key, sequence, block_index, input_id, text, retire, event_id) \
                 SELECT e.session_id, e.branch_id, json_extract(j.value, '$.key'), e.sequence, CAST(j.key AS INTEGER), e.event_id, json_extract(j.value, '$.text'), COALESCE(json_extract(j.value, '$.retire'), 0), e.event_id \
                 FROM timeline_events e, json_each(json_extract(e.body, '$.record.event.data.blocks')) AS j \
                 WHERE e.kind = 'input/message' AND json_valid(e.body) \
                   AND json_type(j.value, '$.key') = 'text' AND json_type(j.value, '$.text') = 'text'",
            )
            .map_err(storage)?;
        }
        let deleted_rows: i64 = tx
            .query_row("SELECT count(*) FROM timeline_deleted_inputs", [], |row| {
                row.get(0)
            })
            .map_err(storage)?;
        if deleted_rows == 0 {
            tx.execute_batch(
                "INSERT OR REPLACE INTO timeline_deleted_inputs(session_id, branch_id, input_id, sequence, event_id) \
                 SELECT session_id, branch_id, json_extract(body, '$.record.event.data.input_id'), sequence, event_id \
                 FROM timeline_events \
                 WHERE kind = 'input/deleted' AND json_valid(body) \
                   AND json_extract(body, '$.record.event.data.input_id') IS NOT NULL",
            )
            .map_err(storage)?;
        }
        let opening_rows: i64 = tx
            .query_row("SELECT count(*) FROM timeline_openings", [], |row| {
                row.get(0)
            })
            .map_err(storage)?;
        if opening_rows == 0 {
            tx.execute_batch(
                "INSERT OR REPLACE INTO timeline_openings(session_id, branch_id, sequence, event_id) \
                 SELECT session_id, branch_id, sequence, event_id FROM timeline_events \
                 WHERE kind IN ('branch/start', 'session/forked')",
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(storage)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(storage)?;
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .map_err(storage)?;
        connection
            .pragma_update(None, "journal_size_limit", WAL_LIMIT_BYTES)
            .map_err(storage)?;
        connection
            .pragma_update(None, "cache_size", -PAGE_CACHE_KIB)
            .map_err(storage)?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(storage)?;
        #[cfg(target_os = "macos")]
        connection
            .pragma_update(None, "fullfsync", true)
            .map_err(storage)?;
        let _ = std::fs::remove_file(startup_note_path(&path));
        let readers = Readers::open(&path)?;
        Ok(Arc::new(Self {
            connection: Mutex::new(Some(connection)),
            readers,
            admission: Mutex::new(Admission {
                closed: false,
                running: 0,
                lease: Some(admission),
            }),
            idle: tokio::sync::Notify::new(),
            path,
            upgraded_from,
        }))
    }
    /// Opens an existing database for one read pass of the Timeline export
    /// (ADR 005 D3), with the format it is in. It takes no lease and runs no
    /// schema statement or upgrade: a daemon may own the database meanwhile.
    /// The pass is one read transaction, so it reads one snapshot; closing
    /// the connection ends it.
    pub fn open_read_only(path: PathBuf) -> Result<(Arc<Self>, u32)> {
        if !path.is_file() {
            return Err(storage(format!(
                "No dimcode database at {}",
                path.display()
            )));
        }
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(storage)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(storage)?;
        connection.execute_batch("BEGIN").map_err(storage)?;
        let unsupported = || {
            storage(format!(
                "unsupported Timeline storage format at {}; the database was not changed",
                path.display()
            ))
        };
        let format = known_format(&schema(&connection)?).ok_or_else(unsupported)?;
        if markers(&connection)? != [i64::from(format)] {
            return Err(unsupported());
        }
        Ok((
            Arc::new(Self {
                connection: Mutex::new(Some(connection)),
                readers: Readers::none(),
                admission: Mutex::new(Admission {
                    closed: false,
                    running: 0,
                    lease: None,
                }),
                idle: tokio::sync::Notify::new(),
                path,
                upgraded_from: None,
            }),
            format,
        ))
    }
    /// The format this open upgraded the store from, once that upgrade has
    /// committed; `None` when the store was created or already current.
    pub fn upgraded_from(&self) -> Option<u32> {
        self.upgraded_from
    }
    /// Runs `operation` on the writer's connection. Only writes, checkpoints
    /// and reads that must follow them run here; business reads use
    /// [`Self::run_read`].
    pub(crate) async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let admitted = self.admit()?;
        tokio::task::spawn_blocking(move || {
            let mut guard = admitted
                .0
                .connection
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let connection = guard
                .as_mut()
                .ok_or_else(|| storage("SQLite storage is closed"))?;
            let result = catch_unwind(AssertUnwindSafe(|| operation(connection)));
            outcome(connection, result)
        })
        .await
        .map_err(storage)?
    }
    /// Runs a read on one of the read-only connections (#599). A read sees the
    /// last commit when its statement starts and does not wait for the
    /// writer's transaction or fsync; each call takes the next connection in
    /// turn, so calls with a long scan, such as the catalog, leave the others
    /// free. The read-only export store's one connection serves both.
    pub(crate) async fn run_read<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let admitted = self.admit()?;
        tokio::task::spawn_blocking(move || {
            let reader = admitted.0.reader();
            match reader {
                Some(guard) => {
                    let connection = guard
                        .as_ref()
                        .ok_or_else(|| storage("SQLite storage is closed"))?;
                    let result = catch_unwind(AssertUnwindSafe(|| operation(connection)));
                    outcome(connection, result)
                }
                None => {
                    let mut guard = admitted
                        .0
                        .connection
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    let connection = guard
                        .as_mut()
                        .ok_or_else(|| storage("SQLite storage is closed"))?;
                    let result = catch_unwind(AssertUnwindSafe(|| operation(connection)));
                    outcome(connection, result)
                }
            }
        })
        .await
        .map_err(storage)?
    }
    fn admit(self: &Arc<Self>) -> Result<Operation> {
        let mut admission = self.admission.lock().unwrap();
        if admission.closed {
            return Err(storage("SQLite storage is closed"));
        }
        admission.running += 1;
        Ok(Operation(self.clone()))
    }
    /// Runs a copy or cleanup on its own write connection (#599). The chunks
    /// of such an operation commit one transaction at a time, so the writer's
    /// commits and publications run between them, and foreign keys stay off
    /// because a fork writes a Session's rows before its session row. The
    /// connection is not the writer's: it waits for SQLite's write lock, like
    /// any second connection, and never holds the process connection mutex.
    pub(crate) async fn run_copy<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let admitted = self.admit()?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let _admitted = admitted;
            let mut connection = Connection::open(&path).map_err(storage)?;
            connection.busy_handler(Some(busy)).map_err(storage)?;
            connection
                .pragma_update(None, "wal_autocheckpoint", 0)
                .map_err(storage)?;
            connection
                .pragma_update(None, "journal_size_limit", WAL_LIMIT_BYTES)
                .map_err(storage)?;
            connection
                .pragma_update(None, "cache_size", -PAGE_CACHE_KIB)
                .map_err(storage)?;
            connection
                .pragma_update(None, "foreign_keys", false)
                .map_err(storage)?;
            #[cfg(target_os = "macos")]
            connection
                .pragma_update(None, "fullfsync", true)
                .map_err(storage)?;
            let result = catch_unwind(AssertUnwindSafe(|| operation(&mut connection)));
            outcome(&connection, result)
        })
        .await
        .map_err(storage)?
    }
    fn reader(&self) -> Option<MutexGuard<'_, Option<Connection>>> {
        let readers = &self.readers;
        if readers.connections.is_empty() {
            return None;
        }
        let index = readers.next.fetch_add(1, Ordering::Relaxed) % readers.connections.len();
        Some(
            readers.connections[index]
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
    pub(crate) async fn run_read_cancellable<T: Send + 'static>(
        self: &Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
        operation: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.run_read(move |connection| {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled("session catalog".into()));
            }
            let interrupt = cancel.clone();
            connection
                .progress_handler(1000, Some(move || interrupt.is_cancelled()))
                .map_err(storage)?;
            let result = catch_unwind(AssertUnwindSafe(|| operation(connection)));
            connection
                .progress_handler(0, None::<fn() -> bool>)
                .map_err(storage)?;
            match result {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(_) if cancel.is_cancelled() => {
                    Err(AgentError::Cancelled("session catalog".into()))
                }
                Ok(result) => result,
            }
        })
        .await
    }

    /// Checkpoints the WAL once it passes `CHECKPOINT_BYTES`. The writer
    /// calls this after a round committed and published, so the copy runs
    /// outside the commit path (#600). A passive checkpoint leaves readers
    /// alone: a read snapshot, such as an export's, keeps the WAL growing
    /// until it ends, and the next call then copies and restarts it.
    pub(super) fn checkpoint(&self) {
        if self.wal_bytes() < CHECKPOINT_BYTES {
            return;
        }
        let mut guard = self
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(connection) = guard.as_mut() else {
            return;
        };
        if let Err(error) = connection.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(())) {
            tracing::error!(db_path = %self.path.display(), phase = "checkpoint", pid = std::process::id(), %error, "storage.transaction");
        }
    }
    fn wal_bytes(&self) -> u64 {
        let mut path = self.path.as_os_str().to_os_string();
        path.push("-wal");
        std::fs::metadata(path).map_or(0, |meta| meta.len())
    }

    pub async fn close(self: &Arc<Self>) -> Result<()> {
        self.admission.lock().unwrap().closed = true;
        let db = self.clone();
        tokio::spawn(async move {
            loop {
                let idle = db.idle.notified();
                tokio::pin!(idle);
                idle.as_mut().enable();
                if db.admission.lock().unwrap().running == 0 {
                    break;
                }
                idle.await;
            }
            tokio::task::spawn_blocking(move || {
                for reader in &db.readers.connections {
                    let mut slot = reader.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(connection) = slot.take()
                        && let Err((connection, failure)) = connection.close()
                    {
                        *slot = Some(connection);
                        return Err(storage(failure));
                    }
                }
                let mut guard = db
                    .connection
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if let Some(connection) = guard.take() {
                    if let Ok(false) = connection.is_readonly("main")
                        && let Err(error) =
                            connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                    {
                        tracing::error!(db_path = %db.path.display(), phase = "close", pid = std::process::id(), %error, "storage.transaction");
                    }
                    if let Err((connection, failure)) = connection.close() {
                        *guard = Some(connection);
                        return Err(storage(failure));
                    }
                }
                let mut admission = db.admission.lock().unwrap();
                if let Some(lease) = &admission.lease {
                    lease.0.unlock().map_err(storage)?;
                }
                admission.lease.take();
                Ok(())
            })
            .await
            .map_err(storage)?
        })
        .await
        .map_err(storage)?
    }
}

/// The result of a connection call, with a panic rolled back so the
/// connection stays usable.
fn outcome<T>(connection: &Connection, result: std::thread::Result<Result<T>>) -> Result<T> {
    match result {
        Ok(result) => result,
        Err(panic) => {
            let failure = storage(format!(
                "SQLite operation panicked: {}",
                panic_message(panic)
            ));
            if !connection.is_autocommit()
                && let Err(cleanup) = connection.execute_batch("ROLLBACK")
            {
                return Err(failure.with_cleanup(storage(cleanup)));
            }
            Err(failure)
        }
    }
}

/// What `vacuum_store` reclaimed, as bytes on disk before and after.
pub struct Vacuumed {
    pub before: u64,
    pub after: u64,
}

/// Reclaims free pages in an existing store by rewriting it with `VACUUM`
/// (#600). It takes the same lease as `open`, so a running daemon makes it
/// fail, and it requires the current format's exact schema: it never creates
/// a store, upgrades one or touches an unsupported format. `VACUUM` rewrites
/// the whole database, so it needs free space about the store's size.
pub fn vacuum_store(path: PathBuf) -> Result<Vacuumed> {
    if !path.is_file() {
        return Err(storage(format!(
            "No dimcode database at {}",
            path.display()
        )));
    }
    let path = path.canonicalize().map_err(storage)?;
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let admission = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(storage)?;
    admission.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => storage(format!(
            "another process owns the SQLite database: {}",
            path.display()
        )),
        std::fs::TryLockError::Error(error) => storage(error),
    })?;
    let _lease = DatabaseLease(admission);
    let connection = Connection::open(&path).map_err(storage)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(storage)?;
    let unsupported = || {
        storage(format!(
            "unsupported Timeline storage format at {}; the database was not changed",
            path.display()
        ))
    };
    if known_format(&schema(&connection)?) != Some(FORMAT)
        || markers(&connection)? != [i64::from(FORMAT)]
    {
        return Err(unsupported());
    }
    let before = store_bytes(&path);
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .map_err(storage)?;
    connection.execute_batch("VACUUM").map_err(storage)?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .map_err(storage)?;
    connection
        .close()
        .map_err(|(_, failure)| storage(failure))?;
    Ok(Vacuumed {
        before,
        after: store_bytes(&path),
    })
}

fn store_bytes(path: &Path) -> u64 {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    [path, Path::new(&wal)]
        .into_iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len())
        .sum()
}

pub struct SqliteStoragePlugin {
    path: PathBuf,
}
impl SqliteStoragePlugin {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}
impl Plugin for SqliteStoragePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "SqliteStorage"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let path = self.path.clone();
        let storage = tokio::task::spawn_blocking(move || SqliteStorage::open(path))
            .await
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))??;
        if let Some(from) = storage.upgraded_from() {
            eprintln!(
                "Upgraded native store {} from format v{from} to v{FORMAT}; earlier dimcode versions cannot open it.",
                storage.path.display()
            );
        }
        let owned = storage.clone();
        if let Err(error) =
            ctx.effect(move || async move { owned.close().await.map_err(Into::into) })
        {
            storage.close().await?;
            return Err(error);
        }
        ctx.provide_as::<SqliteStorage>(storage)?;
        Ok(())
    }
}

pub(super) fn error(error: impl std::fmt::Display) -> AgentError {
    storage(error)
}

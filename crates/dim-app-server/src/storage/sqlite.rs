use dim_agent_runtime::{AgentError, Result, panic_message};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use rutis::host::ErrorKind;
use rutis::{Ctx, Plugin};
use std::{
    fs::File,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

/// The native store format this daemon creates and opens.
const FORMAT: u32 = 25;
const LEGACY_BY_END: (&str, &str) = (
    "timeline_events_by_end",
    "CREATE INDEX timeline_events_by_end ON timeline_events(session_id, branch_id, last_sequence, sequence)",
);
pub(super) const SCHEMA: &[(&str, &str)] = &[
    (
        "agent_store_format",
        "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 25))",
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
        "CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0))",
    ),
    (
        "agents",
        "CREATE TABLE agents(agent_id TEXT PRIMARY KEY, profile TEXT NOT NULL, is_default INTEGER NOT NULL CHECK(is_default IN (0, 1)))",
    ),
    (
        "agents_default_unique",
        "CREATE UNIQUE INDEX agents_default_unique ON agents(is_default) WHERE is_default = 1",
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
        "CREATE TABLE timeline_events (session_id TEXT NOT NULL, branch_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence > 0), last_sequence INTEGER NOT NULL CHECK (last_sequence >= sequence), kind TEXT NOT NULL, body TEXT NOT NULL, event_id TEXT, PRIMARY KEY (session_id, branch_id, sequence), FOREIGN KEY (session_id, branch_id) REFERENCES timeline_branches(session_id, branch_id))",
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
/// The one-way upgrade steps, oldest first: the format a step starts from
/// and the `SCHEMA` entries it adds, which are created in `SCHEMA` order,
/// tables before their indexes. An upgrade also replaces the marker table.
const UPGRADES: [(u32, &[&str]); 11] = [
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
        .chain((version <= 24).then_some(&LEGACY_BY_END))
        .filter(|(name, _)| {
            !UPGRADES
                .iter()
                .any(|(from, added)| *from >= version && added.contains(name))
        })
        .map(|&(name, sql)| match name {
            "agent_store_format" => (name, format_table(version)),
            _ => (name, sql.to_owned()),
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
/// The schema objects of `connection`'s database, sorted by name.
fn schema(connection: &Connection) -> Result<Vec<(String, String)>> {
    let mut query = connection
        .prepare("SELECT name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name")
        .map_err(storage)?;
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(storage)?
        .collect::<rusqlite::Result<_>>()
        .map_err(storage)
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

/// The process connection and maintenance admission. Business modules never
/// open a second connection to this database.
pub struct SqliteStorage {
    pub(super) connection: Mutex<Option<Connection>>,
    admission: Mutex<Admission>,
    idle: tokio::sync::Notify,
    pub(super) path: PathBuf,
    upgraded_from: Option<u32>,
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
            .map_err(storage)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(storage)?;
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
            if markers(&tx)? != [i64::from(version)] {
                return Err(unsupported_version());
            }
            if version != FORMAT {
                tx.execute_batch("DROP TABLE agent_store_format")
                    .map_err(storage)?;
                tx.execute_batch(&format_table(FORMAT)).map_err(storage)?;
                for (from, added) in UPGRADES.iter().filter(|(from, _)| *from >= version) {
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
                }
                tx.execute("INSERT INTO agent_store_format VALUES (?1)", [FORMAT])
                    .map_err(storage)?;
                upgraded_from = Some(version);
            }
        } else {
            return Err(storage(format!(
                "unsupported SQLite schema at {}; the database was preserved. Use a compatible daemon to access it, or choose a new database path for a fresh start.",
                path.display()
            )));
        }
        tx.commit().map_err(storage)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(storage)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(storage)?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(storage)?;
        #[cfg(target_os = "macos")]
        connection
            .pragma_update(None, "fullfsync", true)
            .map_err(storage)?;
        Ok(Arc::new(Self {
            connection: Mutex::new(Some(connection)),
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
    pub(crate) async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        {
            let mut admission = self.admission.lock().unwrap();
            if admission.closed {
                return Err(storage("SQLite storage is closed"));
            }
            admission.running += 1;
        }
        let admitted = Operation(self.clone());
        tokio::task::spawn_blocking(move || {
            let mut guard = admitted
                .0
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let connection = guard
                .as_mut()
                .ok_or_else(|| storage("SQLite storage is closed"))?;
            match catch_unwind(AssertUnwindSafe(|| operation(connection))) {
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
        })
        .await
        .map_err(storage)?
    }
    pub(crate) async fn run_cancellable<T: Send + 'static>(
        self: &Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.run(move |connection| {
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
                let mut guard = db
                    .connection
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(connection) = guard.take()
                    && let Err((connection, failure)) = connection.close()
                {
                    *guard = Some(connection);
                    return Err(storage(failure));
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

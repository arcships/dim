//! The offline legacy prompt-reference migration
//! (`docs/design/prompt-state-read.md` "显式旧引用迁移入口"): `dimcode
//! --migrate-prompt-references --database PATH --session ID --branch NAME`.
//!
//! The store is opened in the current format only: a store an older format
//! left behind is refused rather than upgraded, and one a daemon holds is
//! refused by the same lease `open` takes. Opening a branch's Timeline is all
//! the migration needs, so nothing here builds a Session, mounts a plugin or
//! reaches a host.
use super::{SqliteStorage, SqliteTimelineStore, sqlite};
use dim_agent_runtime::{AgentError, Result, Timeline, TimelineKey, TimelineStore};
use rusqlite::{Connection, OpenFlags};
use rutis::host::CtxHost;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// What one run of [`migrate_prompt_references`] wrote.
pub struct Migrated {
    /// Whether it wrote a saved-reference result. A branch that already has
    /// one, or has no legacy adoption, writes nothing and reports `false`.
    pub written: bool,
}

/// Migrates `session`'s `branch` from legacy prompt adoptions to saved
/// references, exclusively and without upgrading the store.
///
/// A missing database, a store another process holds, an unknown Session or
/// branch, and a store not at the current format all fail without changing the
/// database; a repeated run reports `written: false`. Completion closes the
/// owned database and releases its exclusive lease.
pub async fn migrate_prompt_references(
    path: PathBuf,
    session: String,
    branch: String,
) -> Result<Migrated> {
    if !path.is_file() {
        return Err(AgentError::Storage(format!(
            "No dimcode database at {}",
            path.display()
        )));
    }
    let path = path.canonicalize().map_err(storage)?;
    // Both checks run before the opening that could write: `open` creates an
    // empty store and upgrades an older one, and this entry does neither.
    require_current_format(&path)?;
    require_unowned(&path)?;
    let db = SqliteStorage::open(path).map_err(storage)?;
    let store = SqliteTimelineStore::new(db.clone());
    let migrated = migrate(&store, &session, &branch).await;
    let closed = store.close().await;
    let database_closed = db.close().await;
    let written = migrated?;
    closed.map_err(storage)?;
    database_closed.map_err(storage)?;
    Ok(Migrated { written })
}

fn storage(error: impl std::fmt::Display) -> AgentError {
    AgentError::Storage(error.to_string())
}

/// Refuses a database a running daemon owns, naming what to do about it.
///
/// The probe takes and releases the same `.lock` an opening takes; the opening
/// that follows is what enforces it, so a store that becomes owned in between
/// is still refused, just without this instruction.
fn require_unowned(path: &Path) -> Result<()> {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let admission = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(storage)?;
    match admission.try_lock() {
        Ok(()) => admission.unlock().map_err(storage),
        Err(std::fs::TryLockError::WouldBlock) => Err(storage(format!(
            "another process owns the SQLite database {}; stop the daemon that serves it, then run this again",
            path.display()
        ))),
        Err(std::fs::TryLockError::Error(error)) => Err(storage(error)),
    }
}

/// Refuses a database that is not the current format, without writing to it.
fn require_current_format(path: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(storage)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(storage)?;
    let current = sqlite::is_current_format(&connection).map_err(storage)?;
    connection
        .close()
        .map_err(|(_, failure)| storage(failure))?;
    if current {
        return Ok(());
    }
    Err(storage(format!(
        "unsupported Timeline storage format at {}; the database was not changed",
        path.display()
    )))
}

/// Migrates one branch through the store alone: the branch directory is read
/// once, and its parent chain becomes the ancestors [`Timeline::restore`] is
/// handed. Following that chain, and so refusing a directory that cycles, is
/// this entry's own: `restore` takes the cuts as given and reads no parent.
async fn migrate(store: &Arc<SqliteTimelineStore>, session: &str, branch: &str) -> Result<bool> {
    let directory = store
        .branches(session)
        .await?
        .into_iter()
        .map(|stored| (stored.info.id.clone(), stored.info))
        .collect::<BTreeMap<_, _>>();
    let mut current = directory
        .get(branch)
        .ok_or_else(|| AgentError::UnknownBranch(format!("{session}/{branch}")))?;
    let mut ancestors = Vec::new();
    while let Some(parent) = &current.parent {
        if ancestors.len() >= directory.len() {
            return Err(storage("branch directory cycle"));
        }
        let cut = current
            .branch_point
            .ok_or_else(|| storage(format!("branch {branch} names no branch point")))?;
        ancestors.push((parent.clone(), cut));
        current = directory
            .get(parent)
            .ok_or_else(|| storage(format!("branch {branch} names a missing parent {parent}")))?;
    }
    ancestors.reverse();
    let timeline = Arc::new(
        Timeline::restore(
            rutis::Ctx::root()?,
            TimelineKey {
                session_id: session.to_owned(),
                branch_id: branch.to_owned(),
            },
            ancestors,
            store.clone(),
        )
        .await?,
    );
    let written = dim_agent_capabilities::prompt::migrate_prompt_references(timeline.clone()).await;
    dim_agent_runtime::core::close_timeline(&timeline).await?;
    written
}

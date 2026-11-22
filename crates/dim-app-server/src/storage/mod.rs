mod arrays;
mod backup;
mod catalog;
mod codec;
mod export;
mod face;
mod files;
mod identities;
mod image_upgrade;
mod members;
mod metadata;
mod prompt_migration;
mod query;
mod reclaim;
mod sqlite;
mod text;
mod timeline;

pub use backup::{backup_store, restore_store};
pub use catalog::initialize_session_catalog;
pub(crate) use catalog::{ACTIVITY_KINDS, activity_time, truncate_preview};
#[cfg(feature = "testing")]
pub use codec::NodeLoads;
pub use dim_agent_capabilities::file_access::WorktreeDirectory;
pub use export::export_timeline;
pub use face::Store;
pub use identities::initialize_session_output_id_index;
pub use members::initialize_session_member_index;
#[cfg(feature = "testing")]
pub use metadata::testing;
pub use metadata::{
    MetadataError, SessionMetadata, SessionMetadataService, Title, TitleFields, TitleSource,
};
pub use prompt_migration::{Migrated, migrate_prompt_references};
#[cfg(feature = "testing")]
pub use query::{QueryStatementStats, sqlite_memory};
pub use reclaim::Reclaimed;
pub(crate) use sqlite::AssemblyProgress;
#[cfg(feature = "testing")]
pub use sqlite::set_upgrade_report_delay;
pub use sqlite::{
    FORMAT as STORE_FORMAT, SqliteStorage, SqliteStoragePlugin, StorageReport, Vacuumed,
    vacuum_store,
};
pub(crate) use sqlite::{ImportUser, ReplaceUser, UserDecision, UserDocument, UserWriteError};
pub use text::initialize_session_text_index;
/// Derives every text candidate from the stored rows, in order: what an upgrade
/// from a format before the index ran. A flow test compares a per-row rebuild
/// against it. Feature `testing` only.
#[cfg(feature = "testing")]
pub fn rebuild_text_index(connection: &rusqlite::Connection) -> dim_agent_runtime::Result<()> {
    text::backfill(connection)
}
pub use timeline::{
    PreparedWorktree, SessionStore, SessionStorePlugin, SqliteTimelineStore, TurnWindow,
    WorktreeDirectoryRecord, WorktreeUsage,
};

/// The directory below the database directory where WorkspaceTools keeps each
/// Session's command output, `command-output/<sessionId>/`. The daemon gives
/// it to WorkspaceTools as its output root; a Session delete removes the
/// Session's directory.
pub const COMMAND_OUTPUT_DIRECTORY: &str = "command-output";

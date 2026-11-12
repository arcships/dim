mod arrays;
mod catalog;
mod codec;
mod export;
mod files;
mod identities;
mod members;
mod metadata;
mod query;
mod sqlite;
mod text;
mod timeline;

pub use catalog::initialize_session_catalog;
pub(crate) use catalog::{ACTIVITY_KINDS, truncate_preview};
#[cfg(feature = "testing")]
pub use codec::NodeLoads;
pub use dim_agent_capabilities::file_access::WorktreeDirectory;
pub use export::export_timeline;
pub use files::MAX_SESSION_FILE_BYTES;
pub use identities::initialize_session_output_id_index;
pub use members::initialize_session_member_index;
#[cfg(feature = "testing")]
pub use metadata::testing;
pub use metadata::{MetadataError, SessionMetadata, Title, TitleFields, TitleSource};
#[cfg(feature = "testing")]
pub use query::{QueryStatementStats, sqlite_memory};
#[cfg(feature = "testing")]
pub use sqlite::set_upgrade_report_delay;
pub use sqlite::{
    FORMAT as STORE_FORMAT, SqliteStorage, SqliteStoragePlugin, Vacuumed, vacuum_store,
};
pub use text::initialize_session_text_index;
pub use timeline::{
    SessionStore, SessionStorePlugin, SqliteTimelineStore, TurnWindow, WorktreeUsage,
};

/// The directory below the database directory where WorkspaceTools keeps each
/// Session's command output, `command-output/<sessionId>/`. The daemon gives
/// it to WorkspaceTools as its output root; a Session delete removes the
/// Session's directory.
pub const COMMAND_OUTPUT_DIRECTORY: &str = "command-output";

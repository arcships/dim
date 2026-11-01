mod catalog;
mod codec;
mod export;
mod files;
mod metadata;
mod sqlite;
mod timeline;

pub use catalog::initialize_session_catalog;
pub(crate) use catalog::truncate_preview;
#[cfg(feature = "testing")]
pub use codec::NodeLoads;
pub use export::export_timeline;
#[cfg(feature = "testing")]
pub use metadata::testing;
pub use metadata::{MetadataError, SessionMetadata, Title, TitleFields, TitleSource};
pub use sqlite::{
    FORMAT as STORE_FORMAT, SqliteStorage, SqliteStoragePlugin, Vacuumed, vacuum_store,
};
pub use timeline::{SessionStore, SessionStorePlugin, SqliteTimelineStore, TurnWindow};

/// The directory below the database directory where WorkspaceTools keeps each
/// Session's command output, `command-output/<sessionId>/`. The daemon gives
/// it to WorkspaceTools as its output root; a Session delete removes the
/// Session's directory.
pub const COMMAND_OUTPUT_DIRECTORY: &str = "command-output";

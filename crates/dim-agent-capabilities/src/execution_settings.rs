//! The execution switches the host applies at process start. The storage is a
//! Settings source owned by the App Server; this port has no settings or file
//! dependency.
use rutis::host::BoxFuture;
use std::sync::Arc;

pub const SOURCE: &str = "execution";

pub mod prompt;

/// The document form of the `execution` settings source, and the switches the
/// host reads at process start. It is also the `read` reply of
/// `executionSettingsHost`: the two switches are the whole document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSettings {
    pub sandbox: bool,
    pub network: bool,
}

/// What `executionSettingsHost` fails with: `{code, message}`.
#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ExecutionSettingsError {
    #[error("{message}")]
    Failed { message: String },
}

/// An owner's read of the switches, bound as the face's one operation. The
/// owner bounds it by its own lifetime; a caller ends it by dropping the call.
pub type ExecutionSettingsRead = Arc<
    dyn Fn() -> BoxFuture<'static, Result<ExecutionSettings, ExecutionSettingsError>> + Send + Sync,
>;

/// The `executionSettingsHost` face: its owner provides it with the switches'
/// read, another language calls `read` by name, and a Rust reader calls the
/// same method directly.
#[derive(Clone)]
pub struct ExecutionSettingsHost(ExecutionSettingsRead);

impl ExecutionSettingsHost {
    /// Binds an owner's read as the face's operation.
    pub fn new(read: ExecutionSettingsRead) -> Self {
        Self(read)
    }

    /// Switches that never change, for an assembly that stores none, such as an
    /// evaluation or the architecture graph. The product's source is the App
    /// Server's `ExecutionSettings` part (ADR 011 D1).
    pub fn fixed(settings: ExecutionSettings) -> Self {
        Self::new(Arc::new(move || Box::pin(async move { Ok(settings) })))
    }
}

#[rutis::service]
impl ExecutionSettingsHost {
    /// The switches in effect now.
    pub async fn read(&self) -> std::result::Result<ExecutionSettings, ExecutionSettingsError> {
        (self.0)().await
    }
}

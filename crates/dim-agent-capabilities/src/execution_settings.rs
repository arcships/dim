//! The execution switches the host applies at process start. The storage is a
//! Settings source owned by the App Server; this port has no settings or file
//! dependency.
use crate::port_faces::{self, Empty};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, Key};
use rutis_bridge::session::{HostDispatch, Reply, Value as BridgeValue};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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

/// An owner's read of the switches, as the face's one operation.
pub type ExecutionSettingsRead = Arc<
    dyn Fn(
            CancellationToken,
        ) -> BoxFuture<'static, Result<ExecutionSettings, ExecutionSettingsError>>
        + Send
        + Sync,
>;

/// The `executionSettingsHost` face: `read` is the one operation, served under
/// the name for any language and called directly by a Rust reader through the
/// binding that holds it. The owner provides both with [`provide`].
#[derive(Clone)]
pub struct ExecutionSettingsHost(Arc<dyn HostDispatch>);

impl ExecutionSettingsHost {
    /// Binds an owner's read as the face's operation.
    pub fn new(read: ExecutionSettingsRead) -> Self {
        Self(Arc::new(Face(read)))
    }

    /// Switches that never change, for an assembly that stores none, such as an
    /// evaluation or the architecture graph. The product's source is the App
    /// Server's `ExecutionSettings` part (ADR 011 D1).
    pub fn fixed(settings: ExecutionSettings) -> Self {
        Self::new(Arc::new(move |_| Box::pin(async move { Ok(settings) })))
    }

    /// The switches in effect now.
    pub async fn read(
        &self,
        cancel: CancellationToken,
    ) -> std::result::Result<ExecutionSettings, ExecutionSettingsError> {
        port_faces::call(NAME, &self.0, "read", &Empty {}, &cancel)
            .await
            .map_err(|error| ExecutionSettingsError::Failed {
                message: error.to_string(),
            })
    }
}

/// Provides one owner's [`ExecutionSettingsHost`]: the binding its Rust
/// readers hold and the face any other language calls by name.
pub fn provide(ctx: &Ctx, host: &ExecutionSettingsHost) -> rutis::Result<()> {
    ctx.provide_at(
        Key::<ExecutionSettingsHost>::of(),
        Arc::new(ExecutionSettingsHost(host.0.clone())),
    )?;
    ctx.provide_at(
        port_faces::face_key::<ExecutionSettingsHost>(),
        host.0.clone(),
    )?;
    Ok(())
}

const NAME: &str = "executionSettingsHost";

struct Face(ExecutionSettingsRead);

impl HostDispatch for Face {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"read": "async"}))
    }

    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        if method != "read" {
            return Err(port_faces::unknown_method(NAME, method));
        }
        let Empty {} = port_faces::request(NAME, args)?;
        let cancel = CancellationToken::new();
        let read = (self.0)(cancel.clone());
        port_faces::serving(NAME, cancel, read)
    }
}

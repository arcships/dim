//! The execution switches the host applies at process start. The storage is a
//! Settings source owned by the App Server; this port has no settings or file
//! dependency.
use crate::port_faces::{cancel_arg, cancelled_by, port_failure, unknown_method};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, Key};
use rutis_bridge::session::{HostDispatch, Reply, Value as BridgeValue, host_key};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub const SOURCE: &str = "execution";

pub mod prompt;

/// The document form of the `execution` settings source, and the switches the
/// host reads at process start. It is also the `executionSettingsHost.read`
/// reply: the two switches are the whole document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSettings {
    pub sandbox: bool,
    pub network: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionSettingsError {
    #[error("execution settings: {0}")]
    Failed(String),
}

pub trait ExecutionSettingsSource: Send + Sync + 'static {
    fn read(
        &self,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<ExecutionSettings, ExecutionSettingsError>>;
}

pub struct ExecutionSettingsHost(pub std::sync::Arc<dyn ExecutionSettingsSource>);

impl ExecutionSettingsHost {
    /// Switches that never change, for an assembly that stores none, such as an
    /// evaluation or the architecture graph. The product's source is the App
    /// Server's `ExecutionSettings` part (ADR 011 D1).
    pub fn fixed(settings: ExecutionSettings) -> Self {
        struct Fixed(ExecutionSettings);
        impl ExecutionSettingsSource for Fixed {
            fn read(
                &self,
                _cancel: CancellationToken,
            ) -> BoxFuture<'_, Result<ExecutionSettings, ExecutionSettingsError>> {
                Box::pin(async move { Ok(self.0) })
            }
        }
        Self(std::sync::Arc::new(Fixed(settings)))
    }
}

/// Provides one owner's [`ExecutionSettingsHost`]: the switches the
/// `HostPlatform` reads, and the `executionSettingsHost` face other languages
/// read them by.
pub fn provide(ctx: &Ctx, host: &ExecutionSettingsHost) -> rutis::Result<()> {
    ctx.provide_at(
        Key::<ExecutionSettingsHost>::of(),
        Arc::new(ExecutionSettingsHost(host.0.clone())),
    )?;
    ctx.provide_at(
        host_key(&dim_agent_runtime::names::name_of::<ExecutionSettingsHost>()),
        Arc::new(ExecutionSettingsFace(host.0.clone())) as Arc<dyn HostDispatch>,
    )?;
    Ok(())
}

const NAME: &str = "executionSettingsHost";

struct ExecutionSettingsFace(Arc<dyn ExecutionSettingsSource>);

impl HostDispatch for ExecutionSettingsFace {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({"read": "async"}))
    }

    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        if method != "read" {
            return Err(unknown_method(NAME, method));
        }
        let cancelled = cancel_arg(NAME, args)?;
        let source = self.0.clone();
        Ok(BridgeValue::future(async move {
            let cancel = CancellationToken::new();
            let settings = cancelled_by(&cancel, cancelled, source.read(cancel.clone()))
                .await
                .map_err(|error| port_failure(NAME, "failed", error.to_string()))?;
            rutis_bridge::session::arg(&settings)
        }))
    }
}

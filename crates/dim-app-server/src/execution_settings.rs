//! Execution settings source and the read-only host port.
//!
//! The source is a declared Settings document (`execution.json`, user scope)
//! with the two switches. The App Server owns the source; HostPlatform reads
//! the port at process preparation.
use crate::methods::ManagementMethods;
use crate::protocol::Empty;
use crate::settings::{SettingsScope, SettingsService, SourceDeclaration};
use dim_agent_capabilities::execution_settings::{
    ExecutionSettings, ExecutionSettingsError, ExecutionSettingsHost, ExecutionSettingsSource,
    SOURCE,
};
use dim_agent_capabilities::host_platform::{HostPlatform, SandboxBackend};
use dim_agent_capabilities::methods::{Interruption, MethodDecl, params, reply};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Storage {
    settings: Arc<SettingsService>,
}

impl ExecutionSettingsSource for Storage {
    fn read(
        &self,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<ExecutionSettings, ExecutionSettingsError>> {
        Box::pin(async move {
            let snapshot = self
                .settings
                .read_declared(SOURCE, SettingsScope::User, &cancel)
                .await
                .map_err(|error| ExecutionSettingsError::Failed(error.to_string()))?;
            serde_json::from_value(snapshot.document)
                .map_err(|error| ExecutionSettingsError::Failed(error.to_string()))
        })
    }
}

pub struct ExecutionSettingsPlugin {
    settings: Key<SettingsService>,
}

impl ExecutionSettingsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
        })
    }
}

impl DynPlugin for ExecutionSettingsPlugin {
    fn name(&self) -> &str {
        "ExecutionSettings"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            settings.register_declared(
                ctx,
                SourceDeclaration {
                    source: SOURCE.into(),
                    user_file: "execution.json".into(),
                    project_file: None,
                    schema: json!({
                        "type": "object",
                        "properties": {
                            "sandbox": {"type": "boolean", "default": false},
                            "network": {"type": "boolean", "default": false}
                        },
                        "additionalProperties": false
                    }),
                    user_only: Vec::new(),
                },
            )?;
            let host = ExecutionSettingsHost(Arc::new(Storage { settings }));
            ctx.provide_as::<ExecutionSettingsHost>(Arc::new(host))?;
            Ok(Effect::Done)
        })
    }
}

#[derive(serde::Serialize)]
struct BackendView {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// The read-only management surface HostPlatform's backend availability needs.
/// The switches themselves stay `settings.read`/`settings.update`.
pub struct ExecutionBackendPlugin {
    platform: Key<HostPlatform>,
    methods: Key<ManagementMethods>,
}

impl ExecutionBackendPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            platform: names.key()?,
            methods: names.key()?,
        })
    }
}

impl DynPlugin for ExecutionBackendPlugin {
    fn name(&self) -> &str {
        "ExecutionBackend"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.platform.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let platform = ctx.require_at(&self.platform)?;
            let methods = ctx.require_at(&self.methods)?;
            methods.register(
                ctx,
                "host.execution",
                vec![MethodDecl::new(
                    "host.execution.backend",
                    false,
                    Interruption::Report,
                )],
                Arc::new(move |method, value, _, _| {
                    let platform = platform.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "host.execution.backend" => {
                                let _: Empty = params(value)?;
                                let (available, reason) = match platform.sandbox_backend() {
                                    SandboxBackend::Available => (true, None),
                                    SandboxBackend::Unavailable { reason } => (false, Some(reason)),
                                };
                                reply(BackendView { available, reason })
                            }
                            _ => unreachable!("registered execution backend management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

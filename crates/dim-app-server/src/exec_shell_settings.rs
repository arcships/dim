use crate::{methods::ManagementMethods, protocol::Empty};
use dim_agent_capabilities::{
    host_platform::{ExecShellSettingsError, HostPlatform},
    methods::{Interruption, MethodCall, MethodDecl, RpcError, params, reply},
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateShell {
    expected_revision: String,
    #[serde(default, deserialize_with = "crate::protocol::optional_non_null")]
    executable_path: Option<String>,
}

fn shell_failed(error: ExecShellSettingsError) -> RpcError {
    match error {
        ExecShellSettingsError::Invalid(reason) => RpcError::invalid(reason),
        ExecShellSettingsError::Conflict => {
            RpcError::app(-32000, "exec_shell_conflict", error.to_string())
        }
        error => RpcError::app(-32000, "exec_shell_failed", error.to_string()),
    }
}

pub(crate) struct ExecShellSettingsPlugin {
    platform: Key<HostPlatform>,
    methods: Key<ManagementMethods>,
}

impl ExecShellSettingsPlugin {
    pub(crate) fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            platform: names.key()?,
            methods: names.key()?,
        })
    }
}

impl DynPlugin for ExecShellSettingsPlugin {
    fn name(&self) -> &str {
        "ExecShellSettings"
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
                "host.exec_shell",
                vec![
                    MethodDecl::new("host.exec_shell.get", false, Interruption::Report),
                    MethodDecl::new("host.exec_shell.update", true, Interruption::Report),
                ],
                Arc::new(move |call, stopping, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let platform = platform.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "host.exec_shell.get" => {
                                let _: Empty = params(value)?;
                                reply(
                                    platform
                                        .read_exec_shell_settings(&stopping)
                                        .await
                                        .map_err(shell_failed)?,
                                )
                            }
                            "host.exec_shell.update" => {
                                let p: UpdateShell = params(value)?;
                                reply(
                                    platform
                                        .update_exec_shell_settings(
                                            &p.expected_revision,
                                            p.executable_path,
                                            &stopping,
                                        )
                                        .await
                                        .map_err(shell_failed)?,
                                )
                            }
                            _ => unreachable!("registered exec shell management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

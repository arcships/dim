use crate::{methods::ManagementMethods, protocol::Empty};
use dim_agent_capabilities::{
    host_platform::{HostPlatform, IsolationPolicy, IsolationPolicyError},
    methods::{Interruption, MethodDecl, RpcError, params, reply},
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Update {
    expected_revision: String,
    policy: IsolationPolicy,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reset {
    expected_revision: String,
}

fn failed(error: IsolationPolicyError) -> RpcError {
    let kind = match &error {
        IsolationPolicyError::Conflict => "isolation_policy_conflict",
        _ => "isolation_policy_failed",
    };
    RpcError::app(-32000, kind, error.to_string())
}

pub(crate) struct IsolationPolicyPlugin {
    platform: Key<HostPlatform>,
    methods: Key<ManagementMethods>,
}

impl IsolationPolicyPlugin {
    pub(crate) fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            platform: names.key()?,
            methods: names.key()?,
        })
    }
}

impl DynPlugin for IsolationPolicyPlugin {
    fn name(&self) -> &str {
        "IsolationPolicy"
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
                "host.isolation",
                [
                    "host.isolation.get",
                    "host.isolation.update",
                    "host.isolation.reset",
                ]
                .into_iter()
                .map(|name| {
                    MethodDecl::new(name, name != "host.isolation.get", Interruption::Report)
                })
                .collect(),
                Arc::new(move |method, value, stopping, _| {
                    let platform = platform.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "host.isolation.get" => {
                                let _: Empty = params(value)?;
                                reply(
                                    platform
                                        .read_isolation_policy(&stopping)
                                        .await
                                        .map_err(failed)?,
                                )
                            }
                            "host.isolation.update" => {
                                let p: Update = params(value)?;
                                reply(
                                    platform
                                        .update_isolation_policy(
                                            &p.expected_revision,
                                            p.policy,
                                            &stopping,
                                        )
                                        .await
                                        .map_err(|error| match error {
                                            IsolationPolicyError::Invalid(reason) => {
                                                RpcError::invalid(reason)
                                            }
                                            error => failed(error),
                                        })?,
                                )
                            }
                            "host.isolation.reset" => {
                                let p: Reset = params(value)?;
                                reply(
                                    platform
                                        .reset_isolation_policy(&p.expected_revision, &stopping)
                                        .await
                                        .map_err(failed)?,
                                )
                            }
                            _ => unreachable!("registered isolation policy management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

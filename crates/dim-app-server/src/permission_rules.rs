//! The user's command rules: the `permission` Settings source, its management
//! methods, and the Session binding of Permission's `CommandRulesHost`.
use crate::{
    methods::ManagementMethods,
    protocol::Empty,
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
};
use dim_agent_capabilities::{
    host::{HostError, HostFuture},
    methods::{Interruption, MethodCall, MethodDecl, RpcError, params, reply},
    permission::{CommandRulesHost, ReadCommandRules},
};
use dim_agent_runtime::{AgentBuilder, Names};
use dim_agent_types::{COMMAND_SEPARATORS, CommandRule};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// User scope only: no project file can grant itself a rule.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PermissionSettings {
    #[serde(default)]
    command_rules: Vec<CommandRule>,
}
impl BusinessDocument for PermissionSettings {
    const NAMESPACE: &'static str = "permission";
    const USER_FILE: &'static str = "permission.json";
    fn validate(&self) -> crate::settings::Result<()> {
        for rule in &self.command_rules {
            if rule.command.split_whitespace().next().is_none() {
                return Err(SettingsError::Invalid("empty command rule"));
            }
            if rule.command.contains(COMMAND_SEPARATORS) {
                return Err(SettingsError::Invalid(
                    "command rule contains a shell separator",
                ));
            }
        }
        Ok(())
    }
}

/// Registers the Session part; the host registers `PermissionRulesPlugin`
/// with its Settings.
pub fn install(builder: &mut AgentBuilder) {
    crate::register_names(builder.catalog_mut());
    builder.require_feature(
        dim_agent_runtime::Feature::new("CommandRules").session("CommandRules", |names| {
            Ok(Box::new(CommandRulesPlugin::new(names)?))
        }),
    );
}

/// Reads the user source on each call; nothing is cached.
struct SettingsRules(Arc<SettingsService>);
impl ReadCommandRules for SettingsRules {
    fn rules(&self, cancel: CancellationToken) -> HostFuture<'_, Vec<CommandRule>> {
        Box::pin(async move {
            match self
                .0
                .read::<PermissionSettings>(SettingsScope::User, &cancel)
                .await
            {
                Ok(snapshot) => Ok(snapshot.document.command_rules),
                Err(SettingsError::Cancelled) => {
                    Err(HostError::Cancelled("command rules read cancelled".into()))
                }
                Err(error) => Err(HostError::failed(error.to_string())),
            }
        })
    }
}

struct CommandRulesPlugin {
    host: Key<CommandRulesHost>,
    settings: Key<SettingsService>,
}
impl CommandRulesPlugin {
    fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            host: names.key()?,
            settings: names.key()?,
        })
    }
}
impl DynPlugin for CommandRulesPlugin {
    fn name(&self) -> &str {
        "CommandRules"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            ctx.provide_at(
                self.host.clone(),
                Arc::new(CommandRulesHost(Arc::new(SettingsRules(settings)))),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetRules {
    expected_revision: String,
    command_rules: Vec<CommandRule>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RulesView {
    revision: String,
    command_rules: Vec<CommandRule>,
}
#[derive(Serialize)]
struct Revision {
    revision: String,
}

fn error(error: SettingsError) -> RpcError {
    let kind = match &error {
        SettingsError::Closed => return RpcError::reloading(),
        SettingsError::Cancelled => "operation_interrupted",
        SettingsError::Conflict => "permission_rules_conflict",
        _ => "permission_rules_failed",
    };
    RpcError::app(-32000, kind, error.to_string())
}

/// Process owner of the `permission` source and its management methods.
pub struct PermissionRulesPlugin {
    settings: Key<SettingsService>,
    methods: Key<ManagementMethods>,
}
impl PermissionRulesPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for PermissionRulesPlugin {
    fn name(&self) -> &str {
        "PermissionRules"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let methods = ctx.require_at(&self.methods)?;
            settings.register::<PermissionSettings>(ctx)?;
            methods.register(
                ctx,
                "permission",
                ["permission.rules", "permission.set_rules"]
                    .into_iter()
                    .map(|name| {
                        MethodDecl::new(name, name != "permission.rules", Interruption::Report)
                    })
                    .collect(),
                Arc::new(move |call, stopping, _| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let settings = settings.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "permission.rules" => {
                                let _: Empty = params(value)?;
                                let snapshot = settings
                                    .read::<PermissionSettings>(SettingsScope::User, &stopping)
                                    .await
                                    .map_err(error)?;
                                reply(RulesView {
                                    revision: snapshot.revision,
                                    command_rules: snapshot.document.command_rules,
                                })
                            }
                            "permission.set_rules" => {
                                let p: SetRules = params(value)?;
                                let document = PermissionSettings {
                                    command_rules: p.command_rules,
                                };
                                if let Err(SettingsError::Invalid(reason)) = document.validate() {
                                    return Err(RpcError::invalid(reason));
                                }
                                let snapshot = settings
                                    .update(
                                        SettingsScope::User,
                                        &p.expected_revision,
                                        document,
                                        &stopping,
                                    )
                                    .await
                                    .map_err(error)?;
                                reply(Revision {
                                    revision: snapshot.revision,
                                })
                            }
                            _ => unreachable!("registered permission management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
